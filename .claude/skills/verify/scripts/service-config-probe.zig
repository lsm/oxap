const std = @import("std");
const compat = @import("compat");
const makai = @import("makai");

pub fn main(init: std.process.Init) !void {
    const arena = init.arena.allocator();
    const args = try init.minimal.args.toSlice(arena);
    if (args.len != 2) return error.ConfigPathRequired;
    const stderr = compat.stdio.stderr();
    const environ = try compat.createEnvMap(arena);
    const config = makai.loadServiceConfig(arena, args[1], &environ, stderr) catch std.process.exit(1);
    if (config.agent) |agent| {
        makai.runConfiguredAgent(init.gpa, arena, &environ, agent, compat.stdio.stdin(), compat.stdio.stdout(), stderr) catch std.process.exit(1);
    }
}
