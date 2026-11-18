const std = @import("std");
const compat = @import("compat");
const ai_types = @import("ai_types");
const sse = @import("sse_parser");

const alloc = std.heap.smp_allocator;
const limit = 4 * 1024 * 1024;
var runtime_lock: std.atomic.Mutex = .unlocked;
var runtime: ?*std.Io.Threaded = null;

fn sharedIo() !std.Io {
    while (!runtime_lock.tryLock()) std.atomic.spinLoopHint();
    defer runtime_lock.unlock();
    if (runtime == null) {
        const instance = try alloc.create(std.Io.Threaded);
        instance.* = std.Io.Threaded.init(alloc, .{});
        runtime = instance;
    }
    return runtime.?.io();
}

pub const Reply = struct {
    body: []u8,
    session: ?[]u8,
    challenge: ?[]u8 = null,

    pub fn deinit(self: *Reply, a: std.mem.Allocator) void {
        a.free(self.body);
        if (self.session) |s| a.free(s);
        if (self.challenge) |s| a.free(s);
    }
};

const Job = struct {
    refs: std.atomic.Value(u8) = .init(2),
    cancelled: std.atomic.Value(bool) = .init(false),
    event: std.Io.Event = .unset,
    url: []u8,
    session: ?[]u8,
    initialized: bool,
    body: ?[]u8,
    id: ?u64,
    method: std.http.Method,
    result: ?[]u8 = null,
    new_session: ?[]u8 = null,
    challenge: ?[]u8 = null,
    failure: ?anyerror = null,

    fn release(self: *Job) void {
        if (self.refs.fetchSub(1, .acq_rel) != 1) return;
        alloc.free(self.url);
        if (self.session) |s| alloc.free(s);
        if (self.body) |s| alloc.free(s);
        if (self.result) |s| alloc.free(s);
        if (self.new_session) |s| alloc.free(s);
        if (self.challenge) |s| alloc.free(s);
        alloc.destroy(self);
    }

    fn run(self: *Job, io: std.Io) void {
        defer self.release();
        self.perform(io) catch |err| {
            self.failure = err;
        };
        self.event.set(io);
    }

    fn perform(self: *Job, io: std.Io) !void {
        var client = try compat.http.HttpClient.initWithDefaultProxies(alloc);
        defer client.deinit();
        client.client.io = io;
        try self.exchange(&client, self.method, self.body, self.id, true);
    }

    fn exchange(self: *Job, client: *compat.http.HttpClient, method: std.http.Method, body: ?[]const u8, id: ?u64, primary: bool) anyerror!void {
        if (self.cancelled.load(.acquire)) return error.Cancelled;
        const uri = try std.Uri.parse(self.url);
        var headers: [4]std.http.Header = undefined;
        var count: usize = 0;
        headers[count] = .{ .name = "Accept", .value = "application/json, text/event-stream" };
        count += 1;
        headers[count] = .{ .name = "Content-Type", .value = "application/json" };
        count += 1;
        if (self.session orelse self.new_session) |session| {
            headers[count] = .{ .name = "Mcp-Session-Id", .value = session };
            count += 1;
        }
        if (self.initialized) {
            headers[count] = .{ .name = "MCP-Protocol-Version", .value = "2025-03-26" };
            count += 1;
        }
        var request = try client.openRequest(method, uri, .{ .extra_headers = headers[0..count], .keep_alive = false });
        defer request.deinit();
        if (body) |value| try compat.http.sendRequest(&request, value) else try compat.http.sendBodilessRequest(&request);
        var redirect: [8192]u8 = undefined;
        var response = try compat.http.receiveResponse(&request, &redirect);
        const status = @backingInt(response.head.status);
        if (status == 401 and primary) {
            var auth_headers = response.head.iterateHeaders();
            while (auth_headers.next()) |header| {
                if (std.ascii.eqlIgnoreCase(header.name, "www-authenticate")) {
                    self.challenge = try alloc.dupe(u8, header.value);
                    return;
                }
            }
        }
        if (status < 200 or status >= 300) return error.McpHttpFailure;
        var is_sse = false;
        var iterator = response.head.iterateHeaders();
        while (iterator.next()) |header| {
            if (std.ascii.eqlIgnoreCase(header.name, "content-type")) is_sse = std.mem.startsWith(u8, header.value, "text/event-stream");
            if (primary and std.ascii.eqlIgnoreCase(header.name, "mcp-session-id")) {
                const replacement = try alloc.dupe(u8, header.value);
                if (self.new_session) |old| alloc.free(old);
                self.new_session = replacement;
            }
        }
        if (id == null) return;
        var buffer: [8192]u8 = undefined;
        const reader = compat.http.responseReader(&response, &buffer);
        if (!is_sse) {
            const result = try compat.http.allocRemainingResponse(alloc, reader, limit);
            errdefer alloc.free(result);
            try self.accept(result, id.?);
            return;
        }
        var parser = sse.SSEParser.init(alloc);
        defer parser.deinit();
        var chunk: [4096]u8 = undefined;
        while (true) {
            if (self.cancelled.load(.acquire)) return error.Cancelled;
            const n = try compat.http.readResponse(reader, &chunk);
            if (n == 0) return error.McpServerClosed;
            for (try parser.feed(chunk[0..n])) |event| {
                var value = try std.json.parseFromSlice(std.json.Value, alloc, event.data, .{});
                defer value.deinit();
                if (value.value != .object) return error.InvalidMcpResponse;
                const object = value.value.object;
                if (object.get("method")) |request_method| {
                    if (object.get("id")) |request_id| {
                        const id_json = try std.json.Stringify.valueAlloc(alloc, request_id, .{});
                        defer alloc.free(id_json);
                        const result = if (request_method == .string and std.mem.eql(u8, request_method.string, "ping")) "\"result\":{}" else "\"error\":{\"code\":-32601,\"message\":\"Method not found\"}";
                        const reply = try std.fmt.allocPrint(alloc, "{{\"jsonrpc\":\"2.0\",\"id\":{s},{s}}}", .{ id_json, result });
                        defer alloc.free(reply);
                        try self.exchange(client, .POST, reply, null, false);
                    }
                    continue;
                }
                if (object.get("id")) |response_id| {
                    if (response_id == .integer and response_id.integer >= 0 and response_id.integer == id.?) {
                        self.result = try alloc.dupe(u8, event.data);
                        return;
                    }
                }
            }
        }
    }

    fn accept(self: *Job, result: []u8, id: u64) !void {
        var value = try std.json.parseFromSlice(std.json.Value, alloc, result, .{});
        defer value.deinit();
        if (value.value != .object) return error.InvalidMcpResponse;
        const response_id = value.value.object.get("id") orelse return error.InvalidMcpResponse;
        if (response_id != .integer or response_id.integer < 0 or response_id.integer != id) return error.InvalidMcpResponse;
        self.result = result;
    }
};

fn create(url: []const u8, session: ?[]const u8, initialized: bool, method: std.http.Method, body: ?[]const u8, id: ?u64) !*Job {
    const job = try alloc.create(Job);
    errdefer alloc.destroy(job);
    const url_copy = try alloc.dupe(u8, url);
    errdefer alloc.free(url_copy);
    const session_copy = if (session) |s| try alloc.dupe(u8, s) else null;
    errdefer if (session_copy) |s| alloc.free(s);
    const body_copy = if (body) |s| try alloc.dupe(u8, s) else null;
    errdefer if (body_copy) |s| alloc.free(s);
    job.* = .{ .url = url_copy, .session = session_copy, .initialized = initialized, .body = body_copy, .id = id, .method = method };
    return job;
}

pub fn notify(url: []const u8, session: ?[]const u8, initialized: bool, body: []const u8) void {
    const job = create(url, session, initialized, .POST, body, null) catch return;
    const thread = std.Thread.spawn(.{}, notifyRun, .{job}) catch {
        job.release();
        job.release();
        return;
    };
    thread.detach();
    job.release();
}

fn notifyRun(job: *Job) void {
    defer job.release();
    var response = call(alloc, job.url, job.session, job.initialized, .POST, job.body, null, null, 5000) catch return;
    response.deinit(alloc);
}

pub fn call(a: std.mem.Allocator, url: []const u8, session: ?[]const u8, initialized: bool, method: std.http.Method, body: ?[]const u8, id: ?u64, cancel: ?ai_types.CancelToken, timeout_ms: ?u64) !Reply {
    const io = try sharedIo();
    const job = try create(url, session, initialized, method, body, id);
    var future = io.concurrent(Job.run, .{ job, io }) catch |err| {
        job.release();
        job.release();
        return err;
    };
    defer job.release();
    defer _ = future.await(io);
    const start = std.Io.Timestamp.now(io, .boot).toMilliseconds();
    while (true) {
        if (cancel) |token| if (token.isCancelled()) {
            job.cancelled.store(true, .release);
            _ = future.cancel(io);
            return error.Cancelled;
        };
        if (timeout_ms) |deadline_ms| if (std.Io.Timestamp.now(io, .boot).toMilliseconds() - start >= deadline_ms) {
            job.cancelled.store(true, .release);
            _ = future.cancel(io);
            return error.Timeout;
        };
        const interval: std.Io.Timeout = .{ .duration = .{ .raw = std.Io.Duration.fromMilliseconds(50), .clock = .boot } };
        job.event.waitTimeout(io, interval) catch continue;
        break;
    }
    if (job.failure) |err| return err;
    const result = try a.dupe(u8, job.result orelse "");
    errdefer a.free(result);
    const new_session = if (job.new_session) |s| try a.dupe(u8, s) else null;
    errdefer if (new_session) |s| a.free(s);
    return .{ .body = result, .session = new_session, .challenge = if (job.challenge) |s| try a.dupe(u8, s) else null };
}
