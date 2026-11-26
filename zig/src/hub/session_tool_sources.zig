const std = @import("std");
const builtin = @import("builtin");
const compat = @import("compat");
const contract = @import("contract");

const Grant = struct {
    session_id: []const u8,
    source: struct {
        id: []const u8,
        kind: []const u8,
        protocol: []const u8,
        endpoint: []const u8,
        display_name: []const u8 = "",
    },
    headers: struct { Authorization: []const u8 },
};

pub fn open(path: []const u8) !std.Io.Dir {
    if (comptime builtin.os.tag != .linux and builtin.os.tag != .macos) return error.UnsupportedPlatform;
    const io = compat.fs.defaultIo();
    const dir = try std.Io.Dir.cwd().openDir(io, path, .{ .follow_symlinks = false });
    errdefer dir.close(io);
    if ((try dir.stat(io)).permissions.toMode() & 0o7777 != 0o700 or !ownedByOperator(dir.handle)) return error.InsecurePermissions;
    return dir;
}

fn ownedByOperator(handle: std.Io.File.Handle) bool {
    switch (builtin.os.tag) {
        .linux => {
            const linux = std.os.linux;
            var stat: linux.Statx = undefined;
            return linux.statx(handle, "", linux.AT.EMPTY_PATH, .{ .UID = true }, &stat) == 0 and
                stat.mask.UID and stat.uid == linux.geteuid();
        },
        .macos => {},
        else => return false,
    }
    var stat: std.c.Stat = undefined;
    return std.c.fstat(handle, &stat) == 0 and stat.uid == std.c.geteuid();
}

fn refuse(refusal: *contract.Refusal, id: []const u8) contract.Failure {
    refusal.* = .{ .feature = contract.feature_tool_sources_attach, .reason = contract.reason_unsatisfiable, .source = id };
    return error.UnsupportedFeature;
}

fn text(object: std.json.Value, name: []const u8) []const u8 {
    if (object != .object) return "";
    const value = object.object.get(name) orelse return "";
    return if (value == .string) value.string else "";
}

fn validReference(reference: []const u8) bool {
    const codec = std.base64.url_safe_no_pad;
    var bytes: [32]u8 = undefined;
    if (reference.len != codec.Encoder.calcSize(bytes.len)) return false;
    codec.Decoder.decode(&bytes, reference) catch return false;
    var canonical: [43]u8 = undefined;
    return std.mem.eql(u8, reference, codec.Encoder.encode(&canonical, &bytes));
}

fn equal(left: []const u8, right: []const u8) bool {
    return std.mem.eql(u8, left, right);
}

fn validSource(source: contract.ConfiguredSource) bool {
    if (!equal(source.kind, "remote") or !equal(source.protocol, "mcp")) return false;
    if (source.command.len != 0 or source.args.len != 0 or source.environment.len != 0) return false;
    const uri = std.Uri.parse(source.endpoint) catch return false;
    return (equal(uri.scheme, "http") or equal(uri.scheme, "https")) and
        uri.host != null and uri.user == null and uri.password == null and uri.fragment == null;
}

fn readGrant(arena: std.mem.Allocator, dir: std.Io.Dir, reference: []const u8, session_id: []const u8, id: []const u8) !contract.ConfiguredSource {
    if (comptime builtin.os.tag != .linux and builtin.os.tag != .macos) return error.UnsupportedPlatform;
    const io = compat.fs.defaultIo();
    const listing = try dir.statFile(io, reference, .{ .follow_symlinks = false });
    if (listing.kind != .file or listing.permissions.toMode() & 0o7777 != 0o600 or listing.nlink != 1) return error.InvalidGrant;
    const file = try dir.openFile(io, reference, .{ .follow_symlinks = false, .allow_directory = false });
    defer file.close(io);
    const confirmed = try file.stat(io);
    if (confirmed.inode != listing.inode or confirmed.kind != .file or confirmed.permissions.toMode() & 0o7777 != 0o600 or !ownedByOperator(file.handle)) return error.InvalidGrant;
    var buffer: [4096]u8 = undefined;
    var reader = file.reader(io, &buffer);
    const bytes = try reader.interface.allocRemaining(arena, .limited(32 * 1024));
    const grant = try std.json.parseFromSliceLeaky(Grant, arena, bytes, .{});
    if (!equal(grant.session_id, session_id) or !equal(grant.source.id, id)) return error.InvalidGrant;
    const authorization = grant.headers.Authorization;
    if (!std.mem.startsWith(u8, authorization, "Bearer ")) return error.InvalidGrant;
    const bearer = authorization["Bearer ".len..];
    if (!validReference(bearer)) return error.InvalidGrant;
    for ([_][]const u8{ grant.source.id, grant.source.display_name, grant.source.endpoint }) |published| {
        if (std.mem.indexOf(u8, published, bearer) != null or std.mem.indexOf(u8, published, reference) != null) return error.InvalidGrant;
    }
    const headers = try arena.dupe(std.http.Header, &.{.{ .name = "Authorization", .value = authorization }});
    const source = contract.ConfiguredSource{
        .id = id,
        .kind = grant.source.kind,
        .protocol = grant.source.protocol,
        .endpoint = grant.source.endpoint,
        .display_name = grant.source.display_name,
        .headers = headers,
    };
    if (!validSource(source)) return error.InvalidGrant;
    return source;
}

const Claim = struct {
    reference: []const u8,
    path: []const u8,
    source_id: []const u8,
};

fn claimGrants(io: std.Io, dir: std.Io.Dir, claims: []const Claim, refusal: *contract.Refusal) contract.Failure!void {
    var claimed: usize = 0;
    errdefer for (claims[0..claimed]) |grant| {
        dir.renamePreserve(grant.path, dir, grant.reference, io) catch {};
    };
    for (claims) |grant| {
        dir.renamePreserve(grant.reference, dir, grant.path, io) catch return refuse(refusal, grant.source_id);
        claimed += 1;
    }
    for (claims) |grant| {
        dir.deleteFile(io, grant.path) catch {};
    }
}

pub const Prepared = struct {
    dir: std.Io.Dir,
    sources: []const contract.ConfiguredSource,
    claims: []const Claim,

    pub fn claim(self: Prepared, refusal: *contract.Refusal) contract.Failure!void {
        try claimGrants(compat.fs.defaultIo(), self.dir, self.claims, refusal);
    }
};

pub fn prepare(
    arena: std.mem.Allocator,
    dir: std.Io.Dir,
    registry: []const contract.ConfiguredSource,
    session_id: []const u8,
    reopen: bool,
    raw: []const u8,
    refusal: *contract.Refusal,
) contract.Failure!Prepared {
    const list = std.json.parseFromSliceLeaky([]std.json.Value, arena, raw, .{}) catch |err| {
        if (err == error.OutOfMemory) return error.OutOfMemory;
        return refuse(refusal, "");
    };
    const sources = try arena.alloc(contract.ConfiguredSource, list.len);
    var claims: std.ArrayList(Claim) = .empty;
    for (list, 0..) |entry, index| {
        const id = text(entry, "id");
        if (id.len == 0 or !equal(text(entry, "kind"), "remote") or reopen) return refuse(refusal, id);
        for (sources[0..index]) |previous| {
            if (equal(previous.id, id)) return refuse(refusal, id);
        }
        if (entry.object.contains("credential_ref")) {
            const reference = text(entry, "credential_ref");
            if (entry.object.count() != 3 or session_id.len == 0 or !validReference(reference)) return refuse(refusal, id);
            sources[index] = readGrant(arena, dir, reference, session_id, id) catch |err| {
                if (err == error.OutOfMemory) return error.OutOfMemory;
                return refuse(refusal, id);
            };
            const path = try std.fmt.allocPrint(arena, ".claim-{s}", .{reference});
            try claims.append(arena, .{ .reference = reference, .path = path, .source_id = id });
        } else {
            for (registry) |source| {
                if (!equal(source.id, id)) continue;
                if (!validSource(source)) return refuse(refusal, id);
                sources[index] = source;
                break;
            } else return refuse(refusal, id);
        }
    }
    return .{ .dir = dir, .sources = sources, .claims = claims.items };
}

test "a cleanup error after claiming every grant leaves the references spent" {
    if (comptime builtin.os.tag != .linux and builtin.os.tag != .macos) return error.SkipZigTest;
    const testing = std.testing;
    const io = testing.io;
    var root = testing.tmpDir(.{});
    defer root.cleanup();
    const claims = [_]Claim{
        .{ .reference = "one", .path = ".claim-one", .source_id = "first" },
        .{ .reference = "two", .path = ".claim-two", .source_id = "second" },
    };
    for (claims) |grant| try root.dir.writeFile(io, .{ .sub_path = grant.reference, .data = "fixture" });
    var fault_vtable = io.vtable.*;
    fault_vtable.dirDeleteFile = struct {
        fn delete(_: ?*anyopaque, dir: std.Io.Dir, path: []const u8) std.Io.Dir.DeleteFileError!void {
            if (equal(path, ".claim-two")) return error.AccessDenied;
            return dir.deleteFile(std.testing.io, path);
        }
    }.delete;
    var fault = io;
    fault.vtable = &fault_vtable;
    var refusal: contract.Refusal = .{};
    try claimGrants(fault, root.dir, &claims, &refusal);
    for (claims) |grant| try testing.expectError(error.FileNotFound, root.dir.access(io, grant.reference, .{}));
    try testing.expectError(error.FileNotFound, root.dir.access(io, claims[0].path, .{}));
    try root.dir.access(io, claims[1].path, .{});
}
