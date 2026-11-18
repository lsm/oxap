const std = @import("std");
const compat = @import("compat");
const oauth = @import("mcp_oauth");
const http = @import("mcp_http");
const loopback = @import("oauth/loopback");
const pkce_mod = @import("oauth/pkce");
const storage_mod = @import("oauth/storage");

const max_response = 256 * 1024;
const timeout_ms = 5000;
const json_headers = [_]std.http.Header{.{ .name = "Content-Type", .value = "application/json" }};
const form_headers = [_]std.http.Header{.{ .name = "Content-Type", .value = "application/x-www-form-urlencoded" }};

fn string(value: std.json.Value, name: []const u8) ![]const u8 {
    if (value != .object) return error.InvalidOAuthResponse;
    const member = value.object.get(name) orelse return error.InvalidOAuthResponse;
    if (member != .string or member.string.len == 0) return error.InvalidOAuthResponse;
    return member.string;
}

fn post(a: std.mem.Allocator, url: []const u8, headers: []const std.http.Header, body: []const u8) !std.json.Parsed(std.json.Value) {
    var response = try compat.http.fetch(a, url, .{ .method = .POST, .extra_headers = headers, .body = body, .timeout_ms = timeout_ms, .max_response_bytes = max_response, .no_redirect = true });
    defer response.deinit(a);
    if (response.status < 200 or response.status >= 300) return error.OAuthRequestFailed;
    return std.json.parseFromSlice(std.json.Value, a, response.body, .{ .allocate = .alloc_always });
}

fn addParam(a: std.mem.Allocator, out: *std.ArrayList(u8), name: []const u8, value: []const u8) !void {
    if (out.items.len != 0) try out.append(a, '&');
    try out.appendSlice(a, name);
    try out.append(a, '=');
    const hex = "0123456789ABCDEF";
    for (value) |c| {
        if (std.ascii.isAlphanumeric(c) or c == '-' or c == '.' or c == '_' or c == '~') {
            try out.append(a, c);
        } else {
            try out.appendSlice(a, &.{ '%', hex[c >> 4], hex[c & 15] });
        }
    }
}

fn key(a: std.mem.Allocator, endpoint: []const u8) ![]u8 {
    return std.fmt.allocPrint(a, "mcp:{s}", .{endpoint});
}

pub const Status = struct { authenticated: bool, refreshable: bool };

pub fn status(a: std.mem.Allocator, endpoint: []const u8) !Status {
    _ = try oauth.safeUrl(endpoint, true);
    const id = try key(a, endpoint);
    defer a.free(id);
    var storage = try storage_mod.AuthStorage.loadDefaultStoredOnly(a);
    defer storage.deinit();
    const auth = storage.providers.get(id) orelse return .{ .authenticated = false, .refreshable = false };
    if (auth != .oauth or auth.oauth.provider_data == null) return .{ .authenticated = false, .refreshable = false };
    var data = try std.json.parseFromSlice(std.json.Value, a, auth.oauth.provider_data.?, .{});
    defer data.deinit();
    _ = try string(data.value, "client_id");
    _ = try string(data.value, "issuer");
    _ = try string(data.value, "token_endpoint");
    if (!std.mem.eql(u8, try string(data.value, "resource"), endpoint)) return error.InvalidOAuthResponse;
    return .{ .authenticated = auth.oauth.access.len > 0 and auth.oauth.expires > compat.time.nowMillis(), .refreshable = auth.oauth.refresh.len > 0 };
}

pub fn login(a: std.mem.Allocator, endpoint: []const u8, ctx: *anyopaque, showUrl: *const fn (*anyopaque, []const u8) anyerror!void) !void {
    _ = try oauth.safeUrl(endpoint, true);
    const initialize = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"oxap\",\"version\":\"1\"}}}";
    var reply = try http.call(a, endpoint, null, false, .POST, initialize, 1, null, timeout_ms);
    defer reply.deinit(a);
    const challenge = reply.challenge orelse return error.OAuthNotRequired;
    var metadata = try oauth.discover(a, endpoint, challenge);
    defer metadata.deinit(a);
    const registration_url = metadata.registration_endpoint orelse return error.OAuthRegistrationUnavailable;
    var listener = try loopback.Listener.open();
    defer listener.close();
    const redirect = try listener.redirectUri(a);
    defer a.free(redirect);
    const registration_body = try std.json.Stringify.valueAlloc(a, .{
        .client_name = "oxap",
        .redirect_uris = [_][]const u8{redirect},
        .grant_types = [_][]const u8{ "authorization_code", "refresh_token" },
        .response_types = [_][]const u8{"code"},
        .token_endpoint_auth_method = "none",
    }, .{});
    defer a.free(registration_body);
    var registration = try post(a, registration_url, &json_headers, registration_body);
    defer registration.deinit();
    const client_id = try string(registration.value, "client_id");
    if (!std.mem.eql(u8, try string(registration.value, "token_endpoint_auth_method"), "none")) return error.UnsupportedOAuthClient;
    const pkce = pkce_mod.generatePKCE();
    var state_bytes: [32]u8 = undefined;
    compat.random.fillSecureBytes(&state_bytes);
    var state_buffer: [std.base64.url_safe_no_pad.Encoder.calcSize(state_bytes.len)]u8 = undefined;
    const state = std.base64.url_safe_no_pad.Encoder.encode(&state_buffer, &state_bytes);
    var query: std.ArrayList(u8) = .empty;
    defer query.deinit(a);
    try addParam(a, &query, "response_type", "code");
    try addParam(a, &query, "client_id", client_id);
    try addParam(a, &query, "redirect_uri", redirect);
    try addParam(a, &query, "code_challenge", &pkce.challenge);
    try addParam(a, &query, "code_challenge_method", "S256");
    try addParam(a, &query, "state", state);
    try addParam(a, &query, "resource", metadata.resource);
    if (metadata.scope) |scope| try addParam(a, &query, "scope", scope);
    const authorization = try std.fmt.allocPrint(a, "{s}{s}{s}", .{ metadata.authorization_endpoint, if (std.mem.indexOfScalar(u8, metadata.authorization_endpoint, '?') != null) "&" else "?", query.items });
    defer a.free(authorization);
    try showUrl(ctx, authorization);
    const code = try listener.waitForCode(a, state, neverCancelled, loopback.default_wait_ms);
    defer a.free(code);
    var form: std.ArrayList(u8) = .empty;
    defer form.deinit(a);
    try addParam(a, &form, "grant_type", "authorization_code");
    try addParam(a, &form, "client_id", client_id);
    try addParam(a, &form, "redirect_uri", redirect);
    try addParam(a, &form, "code", code);
    try addParam(a, &form, "code_verifier", &pkce.verifier);
    try addParam(a, &form, "resource", metadata.resource);
    var token = try post(a, metadata.token_endpoint, &form_headers, form.items);
    defer token.deinit();
    const access = try string(token.value, "access_token");
    if (!std.ascii.eqlIgnoreCase(try string(token.value, "token_type"), "Bearer")) return error.UnsupportedOAuthToken;
    const refresh = if (token.value.object.get("refresh_token")) |v| if (v == .string) v.string else return error.InvalidOAuthResponse else "";
    const expires_in = if (token.value.object.get("expires_in")) |v| if (v == .integer and v.integer >= 0) v.integer else return error.InvalidOAuthResponse else null;
    const expires = if (expires_in) |seconds| std.math.add(i64, compat.time.nowMillis(), std.math.mul(i64, seconds, 1000) catch return error.InvalidOAuthResponse) catch return error.InvalidOAuthResponse else std.math.maxInt(i64);
    var credentials: ?storage_mod.Credentials = blk: {
        const provider_data = try std.json.Stringify.valueAlloc(a, .{ .issuer = metadata.issuer, .client_id = client_id, .token_endpoint = metadata.token_endpoint, .resource = metadata.resource, .scope = metadata.scope }, .{});
        errdefer a.free(provider_data);
        const access_copy = try a.dupe(u8, access);
        errdefer a.free(access_copy);
        const refresh_copy = try a.dupe(u8, refresh);
        break :blk .{ .access = access_copy, .refresh = refresh_copy, .expires = expires, .provider_data = provider_data };
    };
    defer if (credentials) |value| value.deinit(a);
    var store = try storage_mod.AuthStorage.loadDefaultStoredOnly(a);
    defer store.deinit();
    const id = try key(a, endpoint);
    defer a.free(id);
    if (store.providers.fetchRemove(id)) |existing| {
        a.free(existing.key);
        existing.value.deinit(a);
    }
    {
        const owned_id = try a.dupe(u8, id);
        errdefer a.free(owned_id);
        try store.providers.put(owned_id, .{ .oauth = credentials.? });
        credentials = null;
    }
    try store.persist();
}

fn neverCancelled() bool {
    return false;
}
