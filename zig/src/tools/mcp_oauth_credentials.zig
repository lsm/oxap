const std = @import("std");
const compat = @import("compat");
const oauth = @import("mcp_oauth");
const login = @import("mcp_oauth_login");
const storage_mod = @import("oauth/storage");

var refresh_lock: std.Io.Mutex = .init;
const headers = [_]std.http.Header{.{ .name = "Content-Type", .value = "application/x-www-form-urlencoded" }};

fn safeBearer(value: []const u8) bool {
    if (value.len == 0 or value[0] == '=') return false;
    var padding = false;
    for (value) |c| {
        if (c == '=') {
            padding = true;
        } else if (padding or !(std.ascii.isAlphanumeric(c) or std.mem.indexOfScalar(u8, "-._~+/", c) != null)) {
            return false;
        }
    }
    return true;
}

fn refresh(old: storage_mod.Credentials, a: std.mem.Allocator) !storage_mod.Credentials {
    if (old.refresh.len == 0) return error.AuthRequired;
    var details = try std.json.parseFromSlice(std.json.Value, a, old.provider_data orelse return error.InvalidOAuthResponse, .{});
    defer details.deinit();
    const resource = try login.string(details.value, "resource");
    const source = try oauth.safeUrl(resource, true);
    const issuer = try login.string(details.value, "issuer");
    const token_url = try login.string(details.value, "token_endpoint");
    _ = try oauth.safeUrl(issuer, oauth.isLoopback(source));
    _ = try oauth.safeUrl(token_url, oauth.isLoopback(source));
    var form: std.ArrayList(u8) = .empty;
    defer {
        std.crypto.secureZero(u8, form.items);
        form.deinit(a);
    }
    try login.addParam(a, &form, "grant_type", "refresh_token");
    try login.addParam(a, &form, "refresh_token", old.refresh);
    try login.addParam(a, &form, "client_id", try login.string(details.value, "client_id"));
    try login.addParam(a, &form, "resource", resource);
    var response = try login.post(a, token_url, &headers, form.items);
    defer response.deinit();
    const access_value = try login.string(response.value, "access_token");
    if (!safeBearer(access_value) or !std.ascii.eqlIgnoreCase(try login.string(response.value, "token_type"), "Bearer")) return error.InvalidOAuthResponse;
    const refresh_value = if (response.value.object.get("refresh_token")) |v| if (v == .string and v.string.len > 0) v.string else return error.InvalidOAuthResponse else old.refresh;
    const expires_in = if (response.value.object.get("expires_in")) |v| if (v == .integer and v.integer >= 0) v.integer else return error.InvalidOAuthResponse else null;
    const expires = if (expires_in) |seconds| std.math.add(i64, compat.time.nowMillis(), std.math.mul(i64, seconds, 1000) catch return error.InvalidOAuthResponse) catch return error.InvalidOAuthResponse else std.math.maxInt(i64);
    const access_copy = try a.dupe(u8, access_value);
    errdefer a.free(access_copy);
    const refresh_copy = try a.dupe(u8, refresh_value);
    errdefer a.free(refresh_copy);
    return .{ .access = access_copy, .refresh = refresh_copy, .expires = expires, .provider_data = try a.dupe(u8, old.provider_data.?) };
}

fn token(credentials: storage_mod.Credentials, a: std.mem.Allocator) ![]const u8 {
    if (!safeBearer(credentials.access)) return error.InvalidOAuthResponse;
    return try a.dupe(u8, credentials.access);
}

const provider = storage_mod.OAuthProvider{ .id = "mcp", .refresh_fn = refresh, .get_api_key_fn = token };

pub fn access(a: std.mem.Allocator, endpoint: []const u8, rejected: ?[]const u8) !?[]u8 {
    const io = std.Io.Threaded.global_single_threaded.io();
    refresh_lock.lockUncancelable(io);
    defer refresh_lock.unlock(io);
    const id = try login.key(a, endpoint);
    defer a.free(id);
    var store = try storage_mod.AuthStorage.loadDefaultStoredOnly(a);
    defer store.deinit();
    const held = store.providers.get(id) orelse return null;
    _ = try oauth.safeUrl(endpoint, true);
    if (held != .oauth) return error.InvalidOAuthResponse;
    var details = try std.json.parseFromSlice(std.json.Value, a, held.oauth.provider_data orelse return error.InvalidOAuthResponse, .{ .allocate = .alloc_always });
    defer details.deinit();
    if (!std.mem.eql(u8, try login.string(details.value, "resource"), endpoint)) return error.InvalidOAuthResource;
    const expired = held.oauth.expires <= compat.time.nowMillis();
    if (expired and storage_mod.renewalFailed(id, held.oauth.refresh)) return error.AuthRequired;
    const current = try store.getApiKey(id, provider) orelse return error.AuthRequired;
    if (rejected) |previous| if (!expired and std.mem.eql(u8, current, previous)) {
        defer {
            std.crypto.secureZero(u8, @constCast(current));
            a.free(current);
        }
        const active = store.providers.get(id).?.oauth;
        if (storage_mod.renewalFailed(id, active.refresh)) return error.AuthRequired;
        try store.refreshCredentials(id, provider);
        const renewed = store.providers.get(id).?.oauth;
        return @constCast(try token(renewed, a));
    };
    return @constCast(current);
}
