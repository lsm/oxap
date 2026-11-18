const std = @import("std");
const mcp = @import("mcp_http");

pub fn main(init: std.process.Init) !void {
    const url = init.environ_map.get("MCP_TEST_URL") orelse return error.MissingUrl;
    var reply = try mcp.call(std.heap.page_allocator, url, null, false, .POST, "{}", 1, null, 5000);
    defer reply.deinit(std.heap.page_allocator);
    const challenge = reply.challenge orelse return error.MissingChallenge;
    if (!std.mem.eql(u8, challenge, "Bearer resource_metadata=\"http://127.0.0.1/metadata\"")) return error.WrongChallenge;
    std.debug.print("PASS: 401 challenge retained\n", .{});
}
