const std = @import("std");
const compat = @import("compat");

const max_metadata = 256 * 1024;
const timeout_ms = 5000;

pub const Metadata = struct {
    resource: []u8,
    issuer: []u8,
    authorization_endpoint: []u8,
    token_endpoint: []u8,
    registration_endpoint: ?[]u8,
    scope: ?[]u8,

    pub fn deinit(self: *Metadata, allocator: std.mem.Allocator) void {
        allocator.free(self.resource);
        allocator.free(self.issuer);
        allocator.free(self.authorization_endpoint);
        allocator.free(self.token_endpoint);
        if (self.registration_endpoint) |value| allocator.free(value);
        if (self.scope) |value| allocator.free(value);
    }
};

pub fn safeUrl(value: []const u8, allow_loopback: bool) !std.Uri {
    const uri = std.Uri.parse(value) catch return error.InvalidOAuthUrl;
    const host = if (uri.host) |component| component.percent_encoded else return error.InvalidOAuthUrl;
    if (host.len == 0 or uri.user != null or uri.password != null or uri.fragment != null) return error.InvalidOAuthUrl;
    const loopback = std.ascii.eqlIgnoreCase(host, "localhost") or std.mem.eql(u8, host, "127.0.0.1") or std.mem.eql(u8, host, "[::1]");
    if (loopback and !allow_loopback) return error.InvalidOAuthUrl;
    if (!std.mem.eql(u8, uri.scheme, "https") and !(loopback and std.mem.eql(u8, uri.scheme, "http"))) return error.InvalidOAuthUrl;
    return uri;
}

fn isLoopback(uri: std.Uri) bool {
    const host = if (uri.host) |component| component.percent_encoded else return false;
    return std.ascii.eqlIgnoreCase(host, "localhost") or std.mem.eql(u8, host, "127.0.0.1") or std.mem.eql(u8, host, "[::1]");
}

fn challengeParam(challenge: []const u8, name: []const u8) !?[]const u8 {
    const prefix = "Bearer ";
    if (challenge.len < prefix.len or !std.ascii.eqlIgnoreCase(challenge[0..prefix.len], prefix)) return error.InvalidOAuthChallenge;
    var start: usize = prefix.len;
    var index = start;
    var quoted = false;
    while (index <= challenge.len) : (index += 1) {
        if (quoted and index < challenge.len and challenge[index] == '\\') {
            if (index + 1 >= challenge.len) return error.InvalidOAuthChallenge;
            index += 1;
            continue;
        }
        if (index < challenge.len and challenge[index] == '"') quoted = !quoted;
        if (index < challenge.len and (challenge[index] != ',' or quoted)) continue;
        if (quoted) return error.InvalidOAuthChallenge;
        const part = challenge[start..index];
        start = index + 1;
        const trimmed = std.mem.trim(u8, part, " \t");
        const eq = std.mem.indexOfScalar(u8, trimmed, '=') orelse continue;
        if (!std.ascii.eqlIgnoreCase(std.mem.trim(u8, trimmed[0..eq], " \t"), name)) continue;
        const value = std.mem.trim(u8, trimmed[eq + 1 ..], " \t");
        if (value.len > 0 and value[0] != '"') {
            for (value) |byte| if (!std.ascii.isAlphanumeric(byte) and std.mem.indexOfScalar(u8, "!#$%&'*+-.^_`|~", byte) == null) return error.InvalidOAuthChallenge;
            return value;
        }
        if (value.len < 3 or value[0] != '"' or value[value.len - 1] != '"') return error.InvalidOAuthChallenge;
        if (std.mem.indexOfAny(u8, value[1 .. value.len - 1], "\\\"") != null) return error.InvalidOAuthChallenge;
        return value[1 .. value.len - 1];
    }
    return null;
}

fn objectString(value: std.json.Value, name: []const u8) ![]const u8 {
    if (value != .object) return error.InvalidOAuthMetadata;
    const field = value.object.get(name) orelse return error.InvalidOAuthMetadata;
    if (field != .string or field.string.len == 0) return error.InvalidOAuthMetadata;
    return field.string;
}

fn fetchJson(a: std.mem.Allocator, url: []const u8) !std.json.Parsed(std.json.Value) {
    var response = try compat.http.fetch(a, url, .{ .timeout_ms = timeout_ms, .max_response_bytes = max_metadata, .no_redirect = true });
    defer response.deinit(a);
    if (response.status != 200) return error.InvalidOAuthMetadata;
    return std.json.parseFromSlice(std.json.Value, a, response.body, .{ .allocate = .alloc_always });
}

pub fn discover(a: std.mem.Allocator, endpoint: []const u8, challenge: []const u8) !Metadata {
    const source_uri = try safeUrl(endpoint, true);
    const allow_loopback = isLoopback(source_uri);
    const metadata_url = (try challengeParam(challenge, "resource_metadata")) orelse return error.InvalidOAuthChallenge;
    _ = try safeUrl(metadata_url, allow_loopback);
    var resource_doc = try fetchJson(a, metadata_url);
    defer resource_doc.deinit();
    const resource = try objectString(resource_doc.value, "resource");
    _ = try safeUrl(resource, allow_loopback);
    if (!std.mem.eql(u8, resource, endpoint)) return error.InvalidOAuthResource;
    var scopes: std.ArrayList(u8) = .empty;
    defer scopes.deinit(a);
    if (try challengeParam(challenge, "scope")) |scope| {
        try scopes.appendSlice(a, scope);
    } else if (resource_doc.value.object.get("scopes_supported")) |supported| {
        if (supported != .array) return error.InvalidOAuthMetadata;
        for (supported.array.items) |item| {
            if (item != .string or item.string.len == 0 or std.mem.indexOfScalar(u8, item.string, ' ') != null) return error.InvalidOAuthMetadata;
            if (scopes.items.len > 0) try scopes.append(a, ' ');
            try scopes.appendSlice(a, item.string);
        }
    }
    const servers = resource_doc.value.object.get("authorization_servers") orelse return error.InvalidOAuthMetadata;
    if (servers != .array or servers.array.items.len == 0 or servers.array.items[0] != .string) return error.InvalidOAuthMetadata;
    const issuer = servers.array.items[0].string;
    const issuer_uri = try safeUrl(issuer, allow_loopback);
    if (issuer_uri.query != null) return error.InvalidOAuthUrl;
    const scheme_end = std.mem.indexOf(u8, issuer, "://") orelse return error.InvalidOAuthUrl;
    const path_start = std.mem.indexOfScalarPos(u8, issuer, scheme_end + 3, '/') orelse issuer.len;
    const metadata = try std.fmt.allocPrint(a, "{s}/.well-known/oauth-authorization-server{s}", .{ issuer[0..path_start], issuer[path_start..] });
    defer a.free(metadata);
    var server_doc = try fetchJson(a, metadata);
    defer server_doc.deinit();
    if (!std.mem.eql(u8, try objectString(server_doc.value, "issuer"), issuer)) return error.InvalidOAuthIssuer;
    if (server_doc.value.object.get("code_challenge_methods_supported")) |methods| {
        if (methods != .array) return error.InvalidOAuthMetadata;
        var supports_s256 = false;
        for (methods.array.items) |method| {
            if (method != .string) return error.InvalidOAuthMetadata;
            if (std.mem.eql(u8, method.string, "S256")) supports_s256 = true;
        }
        if (!supports_s256) return error.UnsupportedOAuthPkce;
    }
    const authorization = try objectString(server_doc.value, "authorization_endpoint");
    const token = try objectString(server_doc.value, "token_endpoint");
    _ = try safeUrl(authorization, allow_loopback);
    _ = try safeUrl(token, allow_loopback);
    const registration = if (server_doc.value.object.get("registration_endpoint")) |value| blk: {
        if (value != .string or value.string.len == 0) return error.InvalidOAuthMetadata;
        _ = try safeUrl(value.string, allow_loopback);
        break :blk value.string;
    } else null;
    const resource_copy = try a.dupe(u8, resource);
    errdefer a.free(resource_copy);
    const issuer_copy = try a.dupe(u8, issuer);
    errdefer a.free(issuer_copy);
    const auth_copy = try a.dupe(u8, authorization);
    errdefer a.free(auth_copy);
    const token_copy = try a.dupe(u8, token);
    errdefer a.free(token_copy);
    const registration_copy = if (registration) |value| try a.dupe(u8, value) else null;
    errdefer if (registration_copy) |value| a.free(value);
    return .{ .resource = resource_copy, .issuer = issuer_copy, .authorization_endpoint = auth_copy, .token_endpoint = token_copy, .registration_endpoint = registration_copy, .scope = if (scopes.items.len > 0) try a.dupe(u8, scopes.items) else null };
}
