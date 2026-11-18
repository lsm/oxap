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
    unauthorized: bool = false,

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
    unauthorized: bool = false,
    authorization: ?[]u8,
    failure: ?anyerror = null,
    headers: []std.http.Header,

    fn release(self: *Job) void {
        if (self.refs.fetchSub(1, .acq_rel) != 1) return;
        alloc.free(self.url);
        if (self.session) |s| alloc.free(s);
        if (self.body) |s| alloc.free(s);
        if (self.result) |s| alloc.free(s);
        if (self.new_session) |s| alloc.free(s);
        if (self.challenge) |s| alloc.free(s);
        if (self.authorization) |s| {
            std.crypto.secureZero(u8, s);
            alloc.free(s);
        }
        for (self.headers) |header| {
            alloc.free(header.name);
            std.crypto.secureZero(u8, @constCast(header.value));
            alloc.free(header.value);
        }
        alloc.free(self.headers);
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
        var headers: std.ArrayList(std.http.Header) = .empty;
        defer headers.deinit(alloc);
        try headers.append(alloc, .{ .name = "Accept", .value = "application/json, text/event-stream" });
        try headers.append(alloc, .{ .name = "Content-Type", .value = "application/json" });
        if (self.session orelse self.new_session) |session| {
            try headers.append(alloc, .{ .name = "Mcp-Session-Id", .value = session });
        }
        if (self.initialized) {
            try headers.append(alloc, .{ .name = "MCP-Protocol-Version", .value = "2025-03-26" });
        }
        try headers.appendSlice(alloc, self.headers);
        if (self.authorization) |value| try headers.append(alloc, .{ .name = "Authorization", .value = value });
        var request = try client.openRequest(method, uri, .{ .extra_headers = headers.items, .keep_alive = false });
        defer request.deinit();
        if (self.authorization != null or self.headers.len > 0) request.redirect_behavior = .unhandled;
        if (body) |value| try compat.http.sendRequest(&request, value) else try compat.http.sendBodilessRequest(&request);
        var redirect: [8192]u8 = undefined;
        var response = try compat.http.receiveResponse(&request, &redirect);
        const status = @backingInt(response.head.status);
        if (status == 401 and primary) {
            self.unauthorized = true;
            var auth_headers = response.head.iterateHeaders();
            while (auth_headers.next()) |header| {
                if (std.ascii.eqlIgnoreCase(header.name, "www-authenticate")) {
                    self.challenge = try alloc.dupe(u8, header.value);
                    break;
                }
            }
            if (self.authorization != null or self.challenge != null) return;
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

fn create(url: []const u8, session: ?[]const u8, initialized: bool, method: std.http.Method, body: ?[]const u8, id: ?u64, access: ?[]const u8, supplied: []const std.http.Header) !*Job {
    const job = try alloc.create(Job);
    errdefer alloc.destroy(job);
    const url_copy = try alloc.dupe(u8, url);
    errdefer alloc.free(url_copy);
    const session_copy = if (session) |s| try alloc.dupe(u8, s) else null;
    errdefer if (session_copy) |s| alloc.free(s);
    const body_copy = if (body) |s| try alloc.dupe(u8, s) else null;
    errdefer if (body_copy) |s| alloc.free(s);
    const headers = try alloc.alloc(std.http.Header, supplied.len);
    var filled: usize = 0;
    errdefer {
        for (headers[0..filled]) |header| {
            alloc.free(header.name);
            std.crypto.secureZero(u8, @constCast(header.value));
            alloc.free(header.value);
        }
        alloc.free(headers);
    }
    for (supplied, headers) |header, *copy| {
        const name = try alloc.dupe(u8, header.name);
        errdefer alloc.free(name);
        copy.* = .{ .name = name, .value = try alloc.dupe(u8, header.value) };
        filled += 1;
    }
    const authorization = if (access) |value| try std.fmt.allocPrint(alloc, "Bearer {s}", .{value}) else null;
    job.* = .{ .url = url_copy, .session = session_copy, .initialized = initialized, .body = body_copy, .id = id, .method = method, .headers = headers, .authorization = authorization };
    return job;
}

pub fn notify(url: []const u8, session: ?[]const u8, initialized: bool, body: []const u8) void {
    notifyWithCredentials(url, session, initialized, body, null, &.{});
}

pub fn notifyWithCredentials(url: []const u8, session: ?[]const u8, initialized: bool, body: []const u8, access: ?[]const u8, headers: []const std.http.Header) void {
    const job = create(url, session, initialized, .POST, body, null, access, headers) catch return;
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
    const access = if (job.authorization) |value| value[7..] else null;
    var response = callWithCredentials(alloc, job.url, job.session, job.initialized, .POST, job.body, null, null, 5000, access, job.headers) catch return;
    response.deinit(alloc);
}

pub fn call(a: std.mem.Allocator, url: []const u8, session: ?[]const u8, initialized: bool, method: std.http.Method, body: ?[]const u8, id: ?u64, cancel: ?ai_types.CancelToken, timeout_ms: ?u64) !Reply {
    return callWithCredentials(a, url, session, initialized, method, body, id, cancel, timeout_ms, null, &.{});
}

pub fn callWithCredentials(a: std.mem.Allocator, url: []const u8, session: ?[]const u8, initialized: bool, method: std.http.Method, body: ?[]const u8, id: ?u64, cancel: ?ai_types.CancelToken, timeout_ms: ?u64, access: ?[]const u8, headers: []const std.http.Header) !Reply {
    const io = try sharedIo();
    const job = try create(url, session, initialized, method, body, id, access, headers);
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
    return .{ .body = result, .session = new_session, .challenge = if (job.challenge) |s| try a.dupe(u8, s) else null, .unauthorized = job.unauthorized };
}
