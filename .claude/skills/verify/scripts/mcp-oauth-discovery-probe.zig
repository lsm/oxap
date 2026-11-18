const std = @import("std");
const oauth = @import("mcp_oauth");

pub fn main(init: std.process.Init) !void {
    const endpoint = init.environ_map.get("MCP_TEST_URL") orelse return error.MissingUrl;
    const challenge = init.environ_map.get("MCP_TEST_CHALLENGE") orelse return error.MissingChallenge;
    var metadata = oauth.discover(std.heap.page_allocator, endpoint, challenge) catch |err| {
        std.debug.print("ERROR: {s}\n", .{@errorName(err)});
        std.process.exit(1);
    };
    defer metadata.deinit(std.heap.page_allocator);
    std.debug.print("PASS: metadata validated\n", .{});
}
