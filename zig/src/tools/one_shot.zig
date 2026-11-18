const std = @import("std");
const compat = @import("compat");
const endpoint_client = @import("endpoint_client");

pub const profile = "open-agent-protocol.agent-control-core";

const envelope_overhead: usize = 4096;
const exit_grace_ms: i64 = 5000;

pub fn run(allocator: std.mem.Allocator, self_path: []const u8, args: []const []const u8, stdin: std.Io.File, stdout: std.Io.File, stderr: std.Io.File) !u8 {
    var failure: ArgError = .none;
    var message: [512]u8 = undefined;
    const options = parseArgs(args, &failure) catch {
        try compat.stdio.writeLine(stderr, describe(failure, &message));
        return 2;
    };
    var arena_state = std.heap.ArenaAllocator.init(allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    const too_long = "oxap run: the prompt and --system text must fit one 1 MiB envelope line";
    const prompt = if (options.prompt == null or std.mem.eql(u8, options.prompt.?, "-")) readAll(arena, stdin) catch |err| switch (err) {
        error.PromptTooLong => {
            try compat.stdio.writeLine(stderr, too_long);
            return 2;
        },
        else => return err,
    } else options.prompt.?;
    if (std.mem.trim(u8, prompt, " \t\r\n").len == 0) {
        try compat.stdio.writeLine(stderr, "oxap run: no prompt; pass one as an argument or on stdin");
        return 2;
    }
    if (!fitsOneLine(prompt, options.system)) {
        try compat.stdio.writeLine(stderr, too_long);
        return 2;
    }

    var client = try endpoint_client.Client.spawn(allocator, .{ .command = self_path, .args = try serveArgs(arena, options) });
    defer client.deinit();
    var driver = Driver.init(arena, options, prompt);
    try client.write(try driver.start());

    const outcome: Outcome = while (true) {
        const frame = client.next(.none) catch |err| break .{ .failed = @errorName(err) };
        const line = switch (frame orelse break .{ .failed = "the endpoint exited before the run ended" }) {
            .envelope => |line| line,
            .control => continue,
        };
        var scratch_state = std.heap.ArenaAllocator.init(allocator);
        defer scratch_state.deinit();
        const step = try driver.handle(scratch_state.allocator(), line);
        if (step.print) |text| {
            try compat.stdio.writeAll(stdout, text);
            if (options.json) try compat.stdio.writeAll(stdout, "\n");
        }
        if (step.outcome) |ended| break ended;
        if (step.send) |sent| client.write(sent) catch |err| break .{ .failed = @errorName(err) };
    };
    if (!options.json) try compat.stdio.writeAll(stdout, driver.finalText());
    client.closeStdin();
    _ = client.waitExit(exit_grace_ms) catch {};
    switch (outcome) {
        .completed => return 0,
        .failed => |reason| {
            const said = std.fmt.bufPrint(&message, "oxap run: {s}", .{reason}) catch reason;
            try compat.stdio.writeLine(stderr, said);
            return 1;
        },
    }
}

fn readAll(arena: std.mem.Allocator, stdin: std.Io.File) ![]const u8 {
    var collected = std.ArrayList(u8).empty;
    var buffer: [64 * 1024]u8 = undefined;
    while (true) {
        const count = compat.stdio.read(stdin, &buffer) catch |err| switch (err) {
            error.EndOfStream => break,
            else => return err,
        };
        if (count == 0) break;
        if (collected.items.len + count > endpoint_client.max_line_bytes) return error.PromptTooLong;
        try collected.appendSlice(arena, buffer[0..count]);
    }
    return collected.items;
}

fn fitsOneLine(prompt: []const u8, system: ?[]const u8) bool {
    const escaped = escapedLength(prompt) +| if (system) |text| escapedLength(text) else 0;
    return escaped +| envelope_overhead < endpoint_client.max_line_bytes;
}

fn escapedLength(text: []const u8) usize {
    var counter = std.Io.Writer.Discarding.init(&.{});
    std.json.Stringify.encodeJsonString(text, .{}, &counter.writer) catch return endpoint_client.max_line_bytes;
    return @intCast(counter.fullCount());
}

pub const Options = struct {
    prompt: ?[]const u8 = null,
    backend: ?[]const u8 = null,
    config: ?[]const u8 = null,
    model: ?[]const u8 = null,
    system: ?[]const u8 = null,
    json: bool = false,
    approve: bool = false,
};

pub const ArgError = union(enum) {
    none,
    missing_value: []const u8,
    repeated_option: []const u8,
    unknown_option: []const u8,
    unexpected_argument: []const u8,
    config_without_backend,
};

pub fn parseArgs(args: []const []const u8, failure: *ArgError) error{InvalidArgument}!Options {
    var options = Options{};
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--json")) {
            options.json = true;
        } else if (std.mem.eql(u8, arg, "--approve")) {
            options.approve = true;
        } else if (valueSlot(&options, arg)) |slot| {
            if (slot.* != null) {
                failure.* = .{ .repeated_option = arg };
                return error.InvalidArgument;
            }
            if (index + 1 >= args.len or std.mem.startsWith(u8, args[index + 1], "--")) {
                failure.* = .{ .missing_value = arg };
                return error.InvalidArgument;
            }
            index += 1;
            slot.* = args[index];
        } else if (std.mem.startsWith(u8, arg, "--")) {
            failure.* = .{ .unknown_option = arg };
            return error.InvalidArgument;
        } else if (options.prompt != null) {
            failure.* = .{ .unexpected_argument = arg };
            return error.InvalidArgument;
        } else {
            options.prompt = arg;
        }
    }
    if (options.config != null and options.backend == null) {
        failure.* = .config_without_backend;
        return error.InvalidArgument;
    }
    return options;
}

fn valueSlot(options: *Options, arg: []const u8) ?*?[]const u8 {
    if (std.mem.eql(u8, arg, "--backend")) return &options.backend;
    if (std.mem.eql(u8, arg, "--config")) return &options.config;
    if (std.mem.eql(u8, arg, "--model")) return &options.model;
    if (std.mem.eql(u8, arg, "--system")) return &options.system;
    return null;
}

pub fn describe(failure: ArgError, buffer: []u8) []const u8 {
    return switch (failure) {
        .none => "oxap run: invalid arguments",
        .missing_value => |flag| std.fmt.bufPrint(buffer, "oxap run: {s} needs a value", .{flag}) catch flag,
        .repeated_option => |flag| std.fmt.bufPrint(buffer, "oxap run: {s} given twice", .{flag}) catch flag,
        .unknown_option => |flag| std.fmt.bufPrint(buffer, "oxap run: unknown option {s}", .{flag}) catch flag,
        .unexpected_argument => |arg| std.fmt.bufPrint(buffer, "oxap run: one prompt only; quote it, or pipe it on stdin: {s}", .{arg}) catch arg,
        .config_without_backend => "oxap run: --config names a backend's registry entry, so it needs --backend",
    };
}

pub fn serveArgs(allocator: std.mem.Allocator, options: Options) ![]const []const u8 {
    var argv = std.ArrayList([]const u8).empty;
    errdefer argv.deinit(allocator);
    try argv.appendSlice(allocator, &.{ "serve", "agent", "--stdio" });
    if (options.backend) |backend| try argv.appendSlice(allocator, &.{ "--backend", backend });
    if (options.config) |config| try argv.appendSlice(allocator, &.{ "--config", config });
    if (options.backend == null) {
        if (options.model) |model| try argv.appendSlice(allocator, &.{ "--model", model });
    }
    return argv.toOwnedSlice(allocator);
}

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
            if (std.mem.eql(u8, kind, "error.response") and std.mem.startsWith(u8, reply_to, request_prefix)) {
                const failure = objectOf(payload, "error");
                const message = stringOf(failure, "message") orelse "no reason given";
                step.outcome = .{ .failed = try std.fmt.allocPrint(self.arena, "the endpoint refused a permission answer: {s}", .{message}) };
            }
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
            if (try self.resolvePermission(payload)) |answer| {
                step.send = answer;
            } else {
                step.outcome = .{ .failed = if (self.options.approve) "the agent offered no choice oxap run reads as approval" else "the agent offered no choice oxap run reads as a denial" };
            }
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

    fn resolvePermission(self: *Driver, payload: ?std.json.ObjectMap) !?[]const u8 {
        const choice_id = pickChoice(payload, if (self.options.approve) &approving_choices else &denying_choices) orelse return null;
        const run_id = stringOf(payload, "run_id") orelse "";
        return try self.encode(try self.mint(), "action.permission.resolve.request", run_id, .{
            .interaction_id = stringOf(payload, "interaction_id") orelse "",
            .requested_by = stringOf(payload, "requested_by") orelse "",
            .responded_by = stringOf(payload, "responded_by") orelse "user",
            .session_id = stringOf(payload, "session_id") orelse self.session_id,
            .run_id = run_id,
            .granted = self.options.approve,
            .choice_id = choice_id,
        });
    }

    fn request(self: *Driver, kind: []const u8, payload: anytype) ![]const u8 {
        const id = try self.mint();
        self.pending = id;
        return self.encode(id, kind, null, payload);
    }

    fn mint(self: *Driver) ![]const u8 {
        self.requests += 1;
        return std.fmt.allocPrint(self.arena, request_prefix ++ "{d}", .{self.requests});
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

const request_prefix = "oxap-run-";

const approving_choices = [_][]const u8{ "approve", "accept", "allow", "allow_once", "yes", "once" };
const denying_choices = [_][]const u8{ "deny", "decline", "reject", "reject_once", "no" };

fn pickChoice(payload: ?std.json.ObjectMap, ranked: []const []const u8) ?[]const u8 {
    const body = payload orelse return null;
    const choices = body.get("choices") orelse return null;
    if (choices != .array) return null;
    for (ranked) |wanted| {
        for (choices.array.items) |choice| {
            if (choice != .object) continue;
            const id = stringOf(choice.object, "id") orelse continue;
            const description = stringOf(choice.object, "description") orelse "";
            if (std.mem.eql(u8, id, wanted) or std.mem.eql(u8, description, wanted)) return id;
        }
    }
    return null;
}

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

test "the built-in loop takes its model from serve agent, and a backend from the submit" {
    const allocator = testing.allocator;
    const builtin_loop = try serveArgs(allocator, .{ .model = "zai-coding-plan/anthropic-messages@glm-5" });
    defer allocator.free(builtin_loop);
    try testing.expectEqual(@as(usize, 5), builtin_loop.len);
    try testing.expectEqualStrings("--model", builtin_loop[3]);
    const harness = try serveArgs(allocator, .{ .backend = "claude", .config = "oap-serve.json", .model = "sonnet" });
    defer allocator.free(harness);
    try testing.expectEqual(@as(usize, 7), harness.len);
    try testing.expectEqualStrings("claude", harness[4]);
    try testing.expectEqualStrings("oap-serve.json", harness[6]);
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
}

test "a permission answer takes the choice each backend names, and a run with no such choice fails instead of waiting" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    const codex = "[{\"id\":\"accept\",\"label\":\"Approve once\"},{\"id\":\"acceptForSession\",\"label\":\"Approve for session\"},{\"id\":\"decline\",\"label\":\"Deny\"},{\"id\":\"cancel\",\"label\":\"Deny and cancel run\"}]";
    const acp = "[{\"id\":\"opt-2\",\"label\":\"Reject\",\"description\":\"reject_once\"},{\"id\":\"opt-1\",\"label\":\"Allow once\",\"description\":\"allow_once\"}]";
    const oapx = "[{\"id\":\"approve\",\"label\":\"Allow once\"},{\"id\":\"approve_always\",\"label\":\"Always allow\"},{\"id\":\"deny\",\"label\":\"Deny\"},{\"id\":\"reject_always\",\"label\":\"Always deny\"}]";
    const cases = [_]struct { choices: []const u8, approve: bool, picks: ?[]const u8 }{
        .{ .choices = codex, .approve = true, .picks = "accept" },
        .{ .choices = codex, .approve = false, .picks = "decline" },
        .{ .choices = acp, .approve = true, .picks = "opt-1" },
        .{ .choices = acp, .approve = false, .picks = "opt-2" },
        .{ .choices = oapx, .approve = true, .picks = "approve" },
        .{ .choices = oapx, .approve = false, .picks = "deny" },
        .{ .choices = "[{\"id\":\"approve_always\",\"label\":\"Always allow\"}]", .approve = false, .picks = null },
        .{ .choices = "[{\"id\":\"reject_always\",\"label\":\"Always deny\"}]", .approve = true, .picks = null },
    };
    for (cases) |case| {
        var driver = Driver.init(arena, .{ .approve = case.approve }, "p");
        _ = try driver.start();
        _ = try drive(arena, &driver, init_reply);
        _ = try drive(arena, &driver, open_reply);
        _ = try drive(arena, &driver, submit_reply);
        const asked = try std.fmt.allocPrint(arena, "{{\"type\":\"action.permission.requested\",\"payload\":{{\"interaction_id\":\"p-1\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"title\":\"Allow\",\"choices\":{s}}}}}", .{case.choices});
        const step = try drive(arena, &driver, asked);
        if (case.picks) |picked| {
            const wanted = try std.fmt.allocPrint(arena, "\"granted\":{},\"choice_id\":\"{s}\"}}}}", .{ case.approve, picked });
            try testing.expect(std.mem.endsWith(u8, step.send.?, wanted));
            try testing.expect(step.outcome == null);
        } else {
            try testing.expect(step.send == null);
            try testing.expect(std.mem.startsWith(u8, step.outcome.?.failed, "the agent offered no choice oxap run reads as"));
        }
    }
}

test "a refused permission answer ends the run instead of leaving it waiting" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var driver = Driver.init(arena, .{}, "p");
    _ = try driver.start();
    _ = try drive(arena, &driver, init_reply);
    _ = try drive(arena, &driver, open_reply);
    _ = try drive(arena, &driver, submit_reply);
    _ = try drive(arena, &driver, "{\"type\":\"action.permission.requested\",\"payload\":{\"interaction_id\":\"p-1\",\"requested_by\":\"agent\",\"responded_by\":\"user\",\"session_id\":\"s-1\",\"run_id\":\"r-1\",\"title\":\"Allow\",\"choices\":[{\"id\":\"deny\",\"label\":\"No\"}]}}");
    const refused = try drive(arena, &driver, "{\"type\":\"error.response\",\"in_reply_to\":\"oxap-run-4\",\"payload\":{\"error\":{\"code\":\"invalid_request\",\"message\":\"not this choice\"}}}");
    try testing.expectEqualStrings("the endpoint refused a permission answer: not this choice", refused.outcome.?.failed);
    const foreign = try drive(arena, &driver, "{\"type\":\"error.response\",\"in_reply_to\":\"someone-else\",\"payload\":{}}");
    try testing.expect(foreign.outcome == null);
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

test "a prompt must fit one envelope line once escaped, counting the system text" {
    const allocator = testing.allocator;
    const plain = try allocator.alloc(u8, 900 * 1024);
    defer allocator.free(plain);
    @memset(plain, 'a');
    try testing.expect(fitsOneLine(plain, null));
    try testing.expect(!fitsOneLine(plain, plain[0 .. 200 * 1024]));
    const controls = try allocator.alloc(u8, 300 * 1024);
    defer allocator.free(controls);
    @memset(controls, 1);
    try testing.expect(!fitsOneLine(controls, null));
}

test "arguments take one prompt, value options once each, and --config only beside --backend" {
    var failure: ArgError = .none;
    const parsed = try parseArgs(&.{ "--backend", "claude", "hello", "--json", "--approve", "--system", "s" }, &failure);
    try testing.expectEqualStrings("hello", parsed.prompt.?);
    try testing.expectEqualStrings("claude", parsed.backend.?);
    try testing.expect(parsed.json and parsed.approve);
    try testing.expect((try parseArgs(&.{}, &failure)).prompt == null);
    try testing.expectError(error.InvalidArgument, parseArgs(&.{ "a", "b" }, &failure));
    try testing.expectEqualStrings("b", failure.unexpected_argument);
    try testing.expectError(error.InvalidArgument, parseArgs(&.{ "--model", "a", "--model", "b" }, &failure));
    try testing.expectEqualStrings("--model", failure.repeated_option);
    try testing.expectError(error.InvalidArgument, parseArgs(&.{ "--model", "--json" }, &failure));
    try testing.expectEqualStrings("--model", failure.missing_value);
    try testing.expectError(error.InvalidArgument, parseArgs(&.{"--agent"}, &failure));
    try testing.expectEqualStrings("--agent", failure.unknown_option);
    try testing.expectError(error.InvalidArgument, parseArgs(&.{ "--config", "x.json" }, &failure));
    try testing.expect(failure == .config_without_backend);
}
