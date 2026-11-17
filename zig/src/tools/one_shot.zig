const std = @import("std");

pub const profile = "open-agent-protocol.agent-control-core";

pub const Options = struct {
    prompt: ?[]const u8 = null,
    backend: ?[]const u8 = null,
    config: ?[]const u8 = null,
    model: ?[]const u8 = null,
    system: ?[]const u8 = null,
    json: bool = false,
    approve: bool = false,
};

pub const Outcome = union(enum) {
    completed,
    failed: []const u8,
};

pub const Step = struct {
    send: ?[]const u8 = null,
    print: ?[]const u8 = null,
    outcome: ?Outcome = null,
};

const Stage = enum { initializing, opening, submitting, running };

pub const Driver = struct {
    arena: std.mem.Allocator,
    options: Options,
    prompt: []const u8,
    stage: Stage = .initializing,
    pending: []const u8 = "",
    requests: u32 = 0,
    revision: ?[]const u8 = null,
    session_id: []const u8 = "",
    printed_text: bool = false,
    ends_in_newline: bool = true,

    pub fn init(arena: std.mem.Allocator, options: Options, prompt: []const u8) Driver {
        return .{ .arena = arena, .options = options, .prompt = prompt };
    }

    pub fn start(self: *Driver) ![]const u8 {
        return self.request("protocol.initialize.request", .{
            .protocol_versions = &[_][]const u8{"0.1"},
            .profiles = &[_][]const u8{profile},
        });
    }

    pub fn finalText(self: *const Driver) []const u8 {
        return if (self.printed_text and !self.ends_in_newline) "\n" else "";
    }

    pub fn handle(self: *Driver, scratch: std.mem.Allocator, line: []const u8) !Step {
        var step = Step{};
        if (self.options.json) step.print = line;
        const parsed = std.json.parseFromSliceLeaky(std.json.Value, scratch, line, .{}) catch return step;
        if (parsed != .object) return step;
        const envelope = parsed.object;
        const kind = stringOf(envelope, "type") orelse return step;
        const payload = objectOf(envelope, "payload");

        if (stringOf(envelope, "in_reply_to")) |reply_to| {
            if (std.mem.eql(u8, reply_to, self.pending)) return self.answered(step, kind, envelope, payload);
            return step;
        }
        if (self.stage != .running and self.stage != .submitting) return step;
        if (std.mem.eql(u8, kind, "content.delta")) {
            const part = objectOf(payload, "part");
            const part_type = stringOf(part, "type") orelse return step;
            if (!std.mem.eql(u8, part_type, "text")) return step;
            const text = stringOf(part, "text") orelse return step;
            if (text.len == 0) return step;
            self.printed_text = true;
            self.ends_in_newline = text[text.len - 1] == '\n';
            if (!self.options.json) step.print = text;
        } else if (std.mem.eql(u8, kind, "action.permission.requested")) {
            step.send = try self.resolvePermission(payload);
        } else if (std.mem.eql(u8, kind, "user.input.requested")) {
            step.outcome = .{ .failed = "the agent asked a question, and oxap run has no one to answer it" };
        } else if (std.mem.eql(u8, kind, "run.completed")) {
            step.outcome = .completed;
        } else if (std.mem.eql(u8, kind, "run.failed")) {
            const failure = objectOf(payload, "error");
            const message = stringOf(failure, "message") orelse "the run failed";
            step.outcome = .{ .failed = try self.arena.dupe(u8, message) };
        } else if (std.mem.eql(u8, kind, "run.cancelled")) {
            step.outcome = .{ .failed = "the run was cancelled" };
        }
        return step;
    }

    fn answered(self: *Driver, given: Step, kind: []const u8, envelope: std.json.ObjectMap, payload: ?std.json.ObjectMap) !Step {
        var step = given;
        if (std.mem.eql(u8, kind, "error.response")) {
            const failure = objectOf(payload, "error");
            const message = stringOf(failure, "message") orelse "the endpoint refused a request";
            step.outcome = .{ .failed = try std.fmt.allocPrint(self.arena, "{s}: {s}", .{ @tagName(self.stage), message }) };
            return step;
        }
        switch (self.stage) {
            .initializing => {
                if (stringOf(envelope, "capability_revision")) |revision| self.revision = try self.arena.dupe(u8, revision);
                self.stage = .opening;
                step.send = try self.request("session.open.request", struct {}{});
            },
            .opening => {
                const session_id = stringOf(payload, "session_id") orelse {
                    step.outcome = .{ .failed = "the endpoint opened a session without an id" };
                    return step;
                };
                self.session_id = try self.arena.dupe(u8, session_id);
                self.stage = .submitting;
                step.send = try self.submit();
            },
            .submitting => {
                const accepted = if (payload) |body| body.get("accepted") else null;
                if (accepted == null or accepted.? != .bool or !accepted.?.bool) {
                    step.outcome = .{ .failed = "the endpoint did not accept the prompt" };
                    return step;
                }
                self.stage = .running;
            },
            .running => {},
        }
        return step;
    }

    fn submit(self: *Driver) ![]const u8 {
        const Message = struct { role: []const u8, content: []const u8 };
        const messages = [_]Message{.{ .role = "user", .content = self.prompt }};
        return self.request("session.message.submit.request", .{
            .session_id = self.session_id,
            .delivery = "auto",
            .messages = &messages,
            .model_id = if (self.options.backend != null) self.options.model else null,
            .instructions = self.options.system,
        });
    }

    fn resolvePermission(self: *Driver, payload: ?std.json.ObjectMap) ![]const u8 {
        const wanted = if (self.options.approve) "approve" else "deny";
        var offered = false;
        if (payload) |body| {
            if (body.get("choices")) |choices| {
                if (choices == .array) {
                    for (choices.array.items) |choice| {
                        if (choice != .object) continue;
                        const id = stringOf(choice.object, "id") orelse continue;
                        if (std.mem.eql(u8, id, wanted)) offered = true;
                    }
                }
            }
        }
        const run_id = stringOf(payload, "run_id") orelse "";
        return self.encode(try self.mint(), "action.permission.resolve.request", run_id, .{
            .interaction_id = stringOf(payload, "interaction_id") orelse "",
            .requested_by = stringOf(payload, "requested_by") orelse "",
            .responded_by = stringOf(payload, "responded_by") orelse "user",
            .session_id = stringOf(payload, "session_id") orelse self.session_id,
            .run_id = run_id,
            .granted = self.options.approve,
            .choice_id = if (offered) wanted else null,
        });
    }

    fn request(self: *Driver, kind: []const u8, payload: anytype) ![]const u8 {
        const id = try self.mint();
        self.pending = id;
        return self.encode(id, kind, null, payload);
    }

    fn mint(self: *Driver) ![]const u8 {
        self.requests += 1;
        return std.fmt.allocPrint(self.arena, "oxap-run-{d}", .{self.requests});
    }

    fn encode(self: *Driver, id: []const u8, kind: []const u8, run_id: ?[]const u8, payload: anytype) ![]const u8 {
        const session_id: ?[]const u8 = if (self.session_id.len > 0) self.session_id else null;
        return std.json.Stringify.valueAlloc(self.arena, .{
            .protocol = "open-agent-protocol",
            .version = "0.1",
            .profile = profile,
            .type = kind,
            .id = id,
            .session_id = session_id,
            .run_id = run_id,
            .capability_revision = if (session_id != null) self.revision else null,
            .payload = payload,
        }, .{ .emit_null_optional_fields = false });
    }
};

fn objectOf(from: ?std.json.ObjectMap, key: []const u8) ?std.json.ObjectMap {
    const object = from orelse return null;
    const value = object.get(key) orelse return null;
    return if (value == .object) value.object else null;
}

fn stringOf(from: ?std.json.ObjectMap, key: []const u8) ?[]const u8 {
    const object = from orelse return null;
    const value = object.get(key) orelse return null;
    return if (value == .string) value.string else null;
}

const testing = std.testing;

const init_reply = "{\"type\":\"protocol.initialize.response\",\"in_reply_to\":\"oxap-run-1\",\"capability_revision\":\"rev-7\",\"payload\":{}}";
const open_reply = "{\"type\":\"session.open.response\",\"in_reply_to\":\"oxap-run-2\",\"payload\":{\"session_id\":\"s-1\",\"status\":\"idle\"}}";
const submit_reply = "{\"type\":\"session.message.submit.response\",\"in_reply_to\":\"oxap-run-3\",\"payload\":{\"session_id\":\"s-1\",\"accepted\":true,\"run_id\":\"r-1\"}}";

fn drive(arena: std.mem.Allocator, driver: *Driver, line: []const u8) !Step {
    return driver.handle(arena, line);
}

test "a run initializes, opens, submits the prompt with its system text, and prints the text it streams" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var driver = Driver.init(arena, .{ .system = "be brief", .backend = "claude", .model = "sonnet" }, "say hi");
    try testing.expectEqualStrings(
        "{\"protocol\":\"open-agent-protocol\",\"version\":\"0.1\",\"profile\":\"open-agent-protocol.agent-control-core\",\"type\":\"protocol.initialize.request\",\"id\":\"oxap-run-1\",\"payload\":{\"protocol_versions\":[\"0.1\"],\"profiles\":[\"open-agent-protocol.agent-control-core\"]}}",
        try driver.start(),
    );
    try testing.expectEqualStrings(
        "{\"protocol\":\"open-agent-protocol\",\"version\":\"0.1\",\"profile\":\"open-agent-protocol.agent-control-core\",\"type\":\"session.open.request\",\"id\":\"oxap-run-2\",\"payload\":{}}",
        (try drive(arena, &driver, init_reply)).send.?,
    );
    try testing.expectEqualStrings(
        "{\"protocol\":\"open-agent-protocol\",\"version\":\"0.1\",\"profile\":\"open-agent-protocol.agent-control-core\",\"type\":\"session.message.submit.request\",\"id\":\"oxap-run-3\",\"session_id\":\"s-1\",\"capability_revision\":\"rev-7\",\"payload\":{\"session_id\":\"s-1\",\"delivery\":\"auto\",\"messages\":[{\"role\":\"user\",\"content\":\"say hi\"}],\"model_id\":\"sonnet\",\"instructions\":\"be brief\"}}",
        (try drive(arena, &driver, open_reply)).send.?,
    );
    const admitted = try drive(arena, &driver, submit_reply);
    try testing.expect(admitted.send == null and admitted.outcome == null);
    const streamed = try drive(arena, &driver, "{\"type\":\"content.delta\",\"payload\":{\"part\":{\"type\":\"text\",\"text\":\"hi\"}}}");
    try testing.expectEqualStrings("hi", streamed.print.?);
    const thought = try drive(arena, &driver, "{\"type\":\"content.delta\",\"payload\":{\"part\":{\"type\":\"reasoning\",\"text\":\"hmm\"}}}");
    try testing.expect(thought.print == null);
    const ended = try drive(arena, &driver, "{\"type\":\"run.completed\",\"payload\":{}}");
    try testing.expect(ended.outcome.? == .completed);
    try testing.expectEqualStrings("\n", driver.finalText());
}

test "json mode prints every line as it came, and the submit names no model for the built-in loop" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var driver = Driver.init(arena, .{ .json = true, .model = "m" }, "p");
    _ = try driver.start();
    try testing.expectEqualStrings(init_reply, (try drive(arena, &driver, init_reply)).print.?);
    const submitted = (try drive(arena, &driver, open_reply)).send.?;
    try testing.expect(std.mem.indexOf(u8, submitted, "model_id") == null);
    try testing.expect(std.mem.indexOf(u8, submitted, "instructions") == null);
    _ = try drive(arena, &driver, submit_reply);
    const delta = "{\"type\":\"content.delta\",\"payload\":{\"part\":{\"type\":\"text\",\"text\":\"x\\n\"}}}";
    try testing.expectEqualStrings(delta, (try drive(arena, &driver, delta)).print.?);
    try testing.expectEqualStrings("", driver.finalText());
}

test "a permission is denied unless --approve, and names a choice only when the endpoint offers it" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    const asked = "{\"type\":\"action.permission.requested\",\"payload\":{\"interaction_id\":\"p-1\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"title\":\"Allow\",\"choices\":[{\"id\":\"approve\",\"label\":\"Yes\"},{\"id\":\"deny\",\"label\":\"No\"}]}}";
    for ([_]bool{ false, true }) |approve| {
        var driver = Driver.init(arena, .{ .approve = approve }, "p");
        _ = try driver.start();
        _ = try drive(arena, &driver, init_reply);
        _ = try drive(arena, &driver, open_reply);
        _ = try drive(arena, &driver, submit_reply);
        const answer = (try drive(arena, &driver, asked)).send.?;
        const expected = if (approve)
            "\"payload\":{\"interaction_id\":\"p-1\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"granted\":true,\"choice_id\":\"approve\"}}"
        else
            "\"payload\":{\"interaction_id\":\"p-1\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"granted\":false,\"choice_id\":\"deny\"}}";
        try testing.expect(std.mem.endsWith(u8, answer, expected));
        try testing.expect(std.mem.startsWith(u8, answer, "{\"protocol\":\"open-agent-protocol\",\"version\":\"0.1\",\"profile\":\"open-agent-protocol.agent-control-core\",\"type\":\"action.permission.resolve.request\",\"id\":\"oxap-run-4\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"capability_revision\":\"rev-7\","));
    }
    var bare = Driver.init(arena, .{}, "p");
    _ = try bare.start();
    _ = try drive(arena, &bare, init_reply);
    _ = try drive(arena, &bare, open_reply);
    _ = try drive(arena, &bare, submit_reply);
    const unoffered = (try drive(arena, &bare, "{\"type\":\"action.permission.requested\",\"payload\":{\"interaction_id\":\"p-2\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"title\":\"Allow\",\"choices\":[{\"id\":\"yes\",\"label\":\"Yes\"}]}}")).send.?;
    try testing.expect(std.mem.indexOf(u8, unoffered, "choice_id") == null);
}

test "a permission asked before the submit is answered leaves the driver waiting on the submit's answer" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var driver = Driver.init(arena, .{}, "p");
    _ = try driver.start();
    _ = try drive(arena, &driver, init_reply);
    _ = try drive(arena, &driver, open_reply);
    const answer = (try drive(arena, &driver, "{\"type\":\"action.permission.requested\",\"payload\":{\"interaction_id\":\"p-1\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"title\":\"Allow\",\"choices\":[{\"id\":\"deny\",\"label\":\"No\"}]}}")).send.?;
    try testing.expect(std.mem.indexOf(u8, answer, "\"id\":\"oxap-run-4\"") != null);
    const refused = try drive(arena, &driver, "{\"type\":\"session.message.submit.response\",\"in_reply_to\":\"oxap-run-3\",\"payload\":{\"accepted\":false}}");
    try testing.expectEqualStrings("the endpoint did not accept the prompt", refused.outcome.?.failed);
}

test "a refusal, a failed run, a cancelled run and a question each end the run as failed" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var refused = Driver.init(arena, .{}, "p");
    _ = try refused.start();
    _ = try drive(arena, &refused, init_reply);
    const open_refused = try drive(arena, &refused, "{\"type\":\"error.response\",\"in_reply_to\":\"oxap-run-2\",\"payload\":{\"error\":{\"code\":\"invalid_request\",\"message\":\"no\"}}}");
    try testing.expectEqualStrings("opening: no", open_refused.outcome.?.failed);

    const endings = [_]struct { line: []const u8, says: []const u8 }{
        .{ .line = "{\"type\":\"run.failed\",\"payload\":{\"error\":{\"code\":\"provider_error\",\"message\":\"quota\"}}}", .says = "quota" },
        .{ .line = "{\"type\":\"run.cancelled\",\"payload\":{}}", .says = "the run was cancelled" },
        .{ .line = "{\"type\":\"user.input.requested\",\"payload\":{}}", .says = "the agent asked a question, and oxap run has no one to answer it" },
    };
    for (endings) |ending| {
        var driver = Driver.init(arena, .{}, "p");
        _ = try driver.start();
        _ = try drive(arena, &driver, init_reply);
        _ = try drive(arena, &driver, open_reply);
        _ = try drive(arena, &driver, submit_reply);
        try testing.expectEqualStrings(ending.says, (try drive(arena, &driver, ending.line)).outcome.?.failed);
    }

    var rejected = Driver.init(arena, .{}, "p");
    _ = try rejected.start();
    _ = try drive(arena, &rejected, init_reply);
    _ = try drive(arena, &rejected, open_reply);
    const not_accepted = try drive(arena, &rejected, "{\"type\":\"session.message.submit.response\",\"in_reply_to\":\"oxap-run-3\",\"payload\":{\"accepted\":false}}");
    try testing.expectEqualStrings("the endpoint did not accept the prompt", not_accepted.outcome.?.failed);
}
