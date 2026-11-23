const std = @import("std");
const json_encode = @import("json_encode");
const service_yaml = @import("service_yaml.zig");

pub const Error = error{ConfigInvalid} || std.mem.Allocator.Error;

pub const Diagnostic = struct {
    message: []const u8 = "",
};

pub const ModelEndpoint = struct {
    base_url: []const u8,
    credential_env: []const u8,
    bearer: bool,
    wire: []const u8 = "",
};

pub const AdapterEntry = struct {
    name: []const u8,
    kind: []const u8,
    executable: []const u8 = "",
    args: []const []const u8 = &.{},
    environment: []const []const u8 = &.{},
    working_directory: ?[]const u8 = null,
    any_directory: bool = false,
    model: []const u8 = "",
    journal_capacity: ?i64 = null,
    allowed_tools: ?[]const []const u8 = null,
    unrestricted_tools: bool = false,
    approval_policy: []const u8 = "",
    sandbox: []const u8 = "",
    provider: []const u8 = "",
    max_tokens: ?i64 = null,
    agent_config_json: ?[]const u8 = null,
    system_prompt: []const u8 = "",
    endpoint: []const u8 = "",
    agent: []const u8 = "",
    model_provider: []const u8 = "",
    model_endpoint: ?ModelEndpoint = null,
};

pub const ToolSource = struct {
    id: []const u8,
    kind: []const u8,
    display_name: []const u8 = "",
    protocol: []const u8 = "",
    endpoint: []const u8 = "",
    command: []const u8 = "",
    args: []const []const u8 = &.{},
    environment: []const []const u8 = &.{},
    headers: []const std.http.Header = &.{},
};

pub const File = struct {
    adapters: []const AdapterEntry = &.{},
    tool_sources: []const ToolSource = &.{},

    pub fn adapter(self: File, name: []const u8) ?AdapterEntry {
        for (self.adapters) |entry| {
            if (std.mem.eql(u8, entry.name, name)) return entry;
        }
        return null;
    }
};

pub const HttpBinding = struct {
    address: []const u8,
    port: u16,
    ipv6: bool,
};

pub const AgentService = struct {
    registry: File,
    binding: union(enum) {
        multi_session_stdio,
        raw_stdio: AdapterEntry,
        http: HttpBinding,
    },
};

pub const ServiceConfig = struct {
    agent: ?AgentService = null,
    provider: ?union(enum) { raw_stdio, http: HttpBinding } = null,
};

pub const Posture = union(enum) {
    unrestricted,
    allowed: []const []const u8,
};

const top_members = [_][]const u8{ "adapters", "tool_sources" };
const adapter_members = [_][]const u8{
    "type",               "executable",      "args",             "environment",
    "working_directory",  "model",           "journal_capacity", "allowed_tools",
    "unrestricted_tools", "approval_policy", "sandbox",          "provider",
    "max_tokens",         "agent_config",    "system_prompt",    "endpoint",
    "agent",              "any_directory",   "model_provider",   "model_endpoint",
};
const source_members = [_][]const u8{ "kind", "display_name", "protocol", "endpoint", "command", "args", "environment", "headers", "auth" };
const source_kinds = [_][]const u8{ "native", "local", "process", "remote", "hosted" };

const Reader = struct {
    arena: std.mem.Allocator,
    diagnostic: *Diagnostic,
    service_config: bool = false,

    fn refuse(self: Reader, comptime format: []const u8, args: anytype) Error {
        self.diagnostic.message = try std.fmt.allocPrint(self.arena, "config: " ++ format, args);
        return error.ConfigInvalid;
    }

    fn members(self: Reader, value: std.json.Value, where: []const u8, known: []const []const u8) Error!?std.json.ObjectMap {
        switch (value) {
            .null => return null,
            .object => |object| {
                const unknown = try self.unknownKeys(object, known);
                if (unknown.len > 0) return self.refuse("{s}: unknown field \"{s}\"", .{ where, unknown[0] });
                return object;
            },
            else => return self.refuse("{s}: must be an object", .{where}),
        }
    }

    fn unknownKeys(self: Reader, object: std.json.ObjectMap, known: []const []const u8) Error![]const []const u8 {
        var unknown = std.ArrayList([]const u8).empty;
        for (object.keys()) |key| {
            if (listed(known, key)) continue;
            try unknown.append(self.arena, key);
        }
        std.mem.sort([]const u8, unknown.items, {}, lessThan);
        return unknown.items;
    }

    fn string(self: Reader, object: std.json.ObjectMap, where: []const u8, key: []const u8) Error![]const u8 {
        const value = object.get(key) orelse return "";
        return switch (value) {
            .null => "",
            .string => |text| text,
            else => self.refuse("{s}: \"{s}\" must be a string", .{ where, key }),
        };
    }

    fn strings(self: Reader, object: std.json.ObjectMap, where: []const u8, key: []const u8) Error!?[]const []const u8 {
        const value = object.get(key) orelse return null;
        const items = switch (value) {
            .null => return null,
            .array => |array| array.items,
            else => return self.refuse("{s}: \"{s}\" must be an array of strings", .{ where, key }),
        };
        const out = try self.arena.alloc([]const u8, items.len);
        for (items, out) |item, *slot| {
            if (item != .string) return self.refuse("{s}: \"{s}\" must be an array of strings", .{ where, key });
            slot.* = item.string;
        }
        return out;
    }

    fn flag(self: Reader, object: std.json.ObjectMap, where: []const u8, key: []const u8) Error!bool {
        const value = object.get(key) orelse return false;
        return switch (value) {
            .null => false,
            .bool => |set| set,
            else => self.refuse("{s}: \"{s}\" must be a boolean", .{ where, key }),
        };
    }

    fn integer(self: Reader, object: std.json.ObjectMap, where: []const u8, key: []const u8) Error!?i64 {
        const value = object.get(key) orelse return null;
        return switch (value) {
            .null => null,
            .integer => |number| number,
            else => self.refuse("{s}: \"{s}\" must be an integer", .{ where, key }),
        };
    }
};

fn readJson(reader: Reader, bytes: []const u8) Error!std.json.Value {
    return std.json.parseFromSliceLeaky(std.json.Value, reader.arena, bytes, .{}) catch |err| {
        if (err == error.OutOfMemory) return error.OutOfMemory;
        return reader.refuse("not one JSON object: {s}", .{@errorName(err)});
    };
}

pub fn parse(arena: std.mem.Allocator, bytes: []const u8, environ: *const std.process.Environ.Map, diagnostic: *Diagnostic) Error!File {
    const reader = Reader{ .arena = arena, .diagnostic = diagnostic };
    const top = try reader.members(try readJson(reader, bytes), "the file", &top_members) orelse return .{};
    return readRegistry(reader, top, environ);
}

pub fn parseService(arena: std.mem.Allocator, bytes: []const u8, environ: *const std.process.Environ.Map, diagnostic: *Diagnostic) Error!ServiceConfig {
    const reader = Reader{ .arena = arena, .diagnostic = diagnostic, .service_config = true };
    var syntax: []const u8 = "";
    const root = service_yaml.parse(arena, bytes, &syntax) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.InvalidYaml => return reader.refuse("{s}", .{syntax}),
    };
    const top = try reader.members(root, "the file", &.{ "agent", "provider", "hub" }) orelse
        return reader.refuse("the file: must be an object", .{});
    if (top.contains("hub")) return reader.refuse("hub is reserved", .{});
    var config = ServiceConfig{};
    if (top.get("agent")) |value| {
        const agent = try reader.members(value, "agent", &.{ "http", "stdio", "adapter", "adapters", "tool_sources" }) orelse
            return reader.refuse("agent: must be an object", .{});
        const binding = try readBinding(reader, agent, "agent");
        if (agent.contains("adapter") and binding != .raw_stdio)
            return reader.refuse("agent.adapter is only valid for stdio: raw", .{});
        const registry = try readRegistry(reader, agent, environ);
        config.agent = .{ .registry = registry, .binding = switch (binding) {
            .multi_session_stdio => .multi_session_stdio,
            .http => |http| .{ .http = http },
            .raw_stdio => raw: {
                const adapter = try reader.string(agent, "agent", "adapter");
                if (adapter.len == 0) return reader.refuse("agent.adapter name is required for stdio: raw", .{});
                const entry = registry.adapter(adapter) orelse
                    return reader.refuse("agent.adapters: no adapter named \"{s}\"", .{adapter});
                break :raw .{ .raw_stdio = entry };
            },
        } };
    }
    if (top.get("provider")) |value| {
        const provider = try reader.members(value, "provider", &.{ "http", "stdio" }) orelse
            return reader.refuse("provider: must be an object", .{});
        config.provider = switch (try readBinding(reader, provider, "provider")) {
            .raw_stdio => .raw_stdio,
            .http => |http| .{ .http = http },
            .multi_session_stdio => unreachable,
        };
    }
    if (config.agent == null and config.provider == null) return reader.refuse("at least one service is required", .{});
    if (config.agent) |agent| if (config.provider) |provider| {
        if (agent.binding == .multi_session_stdio and provider == .raw_stdio)
            return reader.refuse("multi_session stdio requires exclusive use of stdio", .{});
        if (agent.binding == .http and provider == .http) {
            if (!agent.binding.http.ipv6 and agent.binding.http.port == provider.http.port)
                return reader.refuse("Agent and Provider HTTP listeners conflict", .{});
        }
    };
    return config;
}

const Binding = union(enum) { raw_stdio, multi_session_stdio, http: HttpBinding };

fn readBinding(reader: Reader, service: std.json.ObjectMap, name: []const u8) Error!Binding {
    const http = service.contains("http");
    if (http == service.contains("stdio")) return reader.refuse("{s}: exactly one of http or stdio is required", .{name});
    const provider = std.mem.eql(u8, name, "provider");
    if (http) return .{ .http = try readHttp(reader, try reader.string(service, name, "http"), provider) };
    const mode = try reader.string(service, name, "stdio");
    if (std.mem.eql(u8, mode, "raw")) return .raw_stdio;
    if (!provider and std.mem.eql(u8, mode, "multi_session")) return .multi_session_stdio;
    return reader.refuse("invalid {s} stdio binding", .{name});
}

fn readHttp(reader: Reader, address: []const u8, provider: bool) Error!HttpBinding {
    if (address.len == 0) return reader.refuse("HTTP binding requires an explicit address", .{});
    const colon = std.mem.lastIndexOfScalar(u8, address, ':') orelse return reader.refuse("invalid HTTP address; expected host:port", .{});
    const host = address[0..colon];
    const digits = address[colon + 1 ..];
    if (digits.len == 0 or digits.len > 5) return reader.refuse("invalid HTTP address port", .{});
    for (digits) |digit| if (!std.ascii.isDigit(digit)) return reader.refuse("invalid HTTP address port", .{});
    const port = std.fmt.parseInt(u16, digits, 10) catch
        return reader.refuse("invalid HTTP address port", .{});
    const ipv4 = std.mem.eql(u8, host, "127.0.0.1");
    const ipv6 = std.mem.eql(u8, host, "[::1]");
    const localhost = std.mem.eql(u8, host, "localhost");
    if (provider) {
        if (!ipv4 or port == 0) return reader.refuse("Provider HTTP requires 127.0.0.1 and a nonzero port", .{});
    } else if (!ipv4 and !ipv6 and !localhost) {
        return reader.refuse("Agent HTTP requires a loopback address", .{});
    }
    const literal = try std.fmt.allocPrint(reader.arena, "{s}:{d}", .{ if (localhost) "127.0.0.1" else host, port });
    return .{ .address = literal, .port = port, .ipv6 = ipv6 };
}

fn readRegistry(reader: Reader, top: std.json.ObjectMap, environ: *const std.process.Environ.Map) Error!File {
    const arena = reader.arena;
    var adapters = std.ArrayList(AdapterEntry).empty;
    if (top.get("adapters")) |listed_adapters| {
        if (listed_adapters == .object) {
            try adapters.ensureTotalCapacity(arena, listed_adapters.object.count());
            for (try sortedKeys(arena, listed_adapters.object)) |name| {
                adapters.appendAssumeCapacity(try readAdapter(reader, name, listed_adapters.object.get(name).?, environ));
            }
        } else if (listed_adapters != .null) {
            return reader.refuse("adapters: must be an object", .{});
        }
    }

    var sources = std.ArrayList(ToolSource).empty;
    if (top.get("tool_sources")) |listed_sources| {
        if (listed_sources == .object) {
            try sources.ensureTotalCapacity(arena, listed_sources.object.count());
            for (try sortedKeys(arena, listed_sources.object)) |id| {
                sources.appendAssumeCapacity(try readToolSource(reader, id, listed_sources.object.get(id).?, environ));
            }
        } else if (listed_sources != .null) {
            return reader.refuse("tool_sources: must be an object", .{});
        }
    }
    return .{ .adapters = adapters.items, .tool_sources = sources.items };
}

fn readAdapter(reader: Reader, name: []const u8, value: std.json.Value, environ: *const std.process.Environ.Map) Error!AdapterEntry {
    const where = try std.fmt.allocPrint(reader.arena, "adapter \"{s}\"", .{name});
    const object = try reader.members(value, where, &adapter_members) orelse {
        if (reader.service_config) try validateServiceAdapter(reader, where, name, .{});
        return .{ .name = name, .kind = name };
    };
    const declared = try reader.string(object, where, "type");
    const kind = if (declared.len > 0) declared else name;
    if (reader.service_config) try validateServiceAdapter(reader, where, kind, object);
    const model_provider = try reader.string(object, where, "model_provider");
    if (model_provider.len > 0 and !std.mem.eql(u8, kind, "claude") and !std.mem.eql(u8, kind, "codex") and !std.mem.eql(u8, kind, "deepseek")) {
        return reader.refuse("{s}: \"model_provider\" routes only claude, codex, and deepseek adapters; a {s} adapter has no provider setting oxap translates it into", .{ where, kind });
    }
    const args = try reader.strings(object, where, "args") orelse &.{};
    const model = try reader.string(object, where, "model");
    const listed_environment = try reader.strings(object, where, "environment") orelse &.{};
    const model_endpoint = if (object.get("model_endpoint")) |raw|
        try readModelEndpoint(reader, where, kind, raw, model_provider, listed_environment, environ)
    else
        null;
    if (model_endpoint != null) {
        const pi = std.mem.eql(u8, kind, "pi");
        if (std.mem.eql(u8, kind, "hermes") and hermesModelBlank(model))
            return reader.refuse("{s}: Hermes model_endpoint requires a model id", .{where});
        if ((pi or std.mem.eql(u8, kind, "opencode")) and
            (!std.mem.startsWith(u8, model, "oapx/") or model.len == "oapx/".len))
            return reader.refuse("{s}: model_endpoint requires model oapx/<model-id>", .{where});
        if (pi) for (args) |arg| {
            const flag = arg[0 .. std.mem.indexOfScalar(u8, arg, '=') orelse arg.len];
            if (std.mem.eql(u8, flag, "--model") or std.mem.eql(u8, flag, "--provider") or std.mem.eql(u8, flag, "--api-key"))
                return reader.refuse("{s}: args pass {s}, which model_endpoint controls", .{ where, flag });
        };
    }
    const environment = try resolveEnvironment(reader, where, listed_environment, environ, false);
    const allowed_tools = try reader.strings(object, where, "allowed_tools");
    var agent_config_json: ?[]const u8 = null;
    if (object.get("agent_config")) |raw| {
        if (raw != .null) agent_config_json = try json_encode.valueAlloc(reader.arena, raw);
    }
    return .{
        .name = name,
        .kind = kind,
        .executable = try reader.string(object, where, "executable"),
        .args = args,
        .environment = environment,
        .working_directory = nonEmpty(try reader.string(object, where, "working_directory")),
        .any_directory = try reader.flag(object, where, "any_directory"),
        .model = model,
        .journal_capacity = try reader.integer(object, where, "journal_capacity"),
        .allowed_tools = allowed_tools,
        .unrestricted_tools = try reader.flag(object, where, "unrestricted_tools"),
        .approval_policy = try reader.string(object, where, "approval_policy"),
        .sandbox = try reader.string(object, where, "sandbox"),
        .provider = try reader.string(object, where, "provider"),
        .max_tokens = try reader.integer(object, where, "max_tokens"),
        .agent_config_json = agent_config_json,
        .system_prompt = try reader.string(object, where, "system_prompt"),
        .endpoint = try reader.string(object, where, "endpoint"),
        .agent = try reader.string(object, where, "agent"),
        .model_provider = model_provider,
        .model_endpoint = model_endpoint,
    };
}

const ServiceAdapterKind = enum { memory, oapx, claude, codex, pi, acp, hermes, deepseek, opencode };

fn validateServiceAdapter(reader: Reader, where: []const u8, name: []const u8, object: std.json.ObjectMap) Error!void {
    const kind = std.meta.stringToEnum(ServiceAdapterKind, name) orelse
        return reader.refuse("{s}: oxap does not know adapter type \"{s}\"", .{ where, name });
    const specific: []const []const u8 = switch (kind) {
        .memory => &.{},
        .oapx => &.{"environment"},
        .claude => &.{ "executable", "args", "environment", "working_directory", "model", "allowed_tools", "unrestricted_tools", "model_provider", "model_endpoint" },
        .codex => &.{ "executable", "args", "environment", "working_directory", "model", "approval_policy", "sandbox", "endpoint", "model_provider", "model_endpoint" },
        .pi => &.{ "executable", "args", "environment", "working_directory", "model", "model_endpoint" },
        .acp => &.{ "executable", "args", "environment", "working_directory" },
        .hermes => &.{ "executable", "args", "environment", "working_directory", "model", "model_endpoint" },
        .deepseek => &.{ "executable", "args", "environment", "working_directory", "model", "provider", "max_tokens", "model_provider", "model_endpoint" },
        .opencode => &.{ "executable", "environment", "working_directory", "endpoint", "agent", "model", "model_endpoint" },
    };
    for (object.keys()) |key| {
        if (!listed(&.{ "type", "any_directory", "journal_capacity", "agent_config", "system_prompt" }, key) and !listed(specific, key))
            return reader.refuse("{s}: unknown field \"{s}\" for type \"{s}\"", .{ where, key, name });
    }
    if (try reader.integer(object, where, "journal_capacity")) |capacity| {
        if (capacity < 0) return reader.refuse("{s}: journal_capacity must not be negative", .{where});
    }
}

fn readModelEndpoint(
    reader: Reader,
    where: []const u8,
    kind: []const u8,
    raw: std.json.Value,
    provider: []const u8,
    environment: []const []const u8,
    environ: *const std.process.Environ.Map,
) Error!ModelEndpoint {
    const claude = std.mem.eql(u8, kind, "claude");
    const codex = std.mem.eql(u8, kind, "codex");
    const pi = std.mem.eql(u8, kind, "pi");
    const opencode = std.mem.eql(u8, kind, "opencode");
    const hermes = std.mem.eql(u8, kind, "hermes");
    if (!claude and !codex and !pi and !opencode and !hermes and !std.mem.eql(u8, kind, "deepseek"))
        return reader.refuse("{s}: \"model_endpoint\" routes only claude, codex, deepseek, hermes, opencode, and pi adapters; a {s} adapter has no custom endpoint translation", .{ where, kind });
    const fixed_wire = if (codex or hermes) "openai-responses" else "anthropic-messages";
    const destinations: []const []const u8 =
        if (pi) &.{pi_endpoint_variable} else if (opencode) &opencode_endpoint_variables else if (claude) &claude_endpoint_variables else if (codex) &.{codex_endpoint_variable} else if (hermes) &hermes_endpoint_variables else &deepseek_endpoint_variables;
    if (provider.len > 0)
        return reader.refuse("{s}: \"model_provider\" and \"model_endpoint\" name two providers", .{where});
    const object = try reader.members(raw, where, &.{ "base_url", "credential_env", "wire", "auth" }) orelse
        return reader.refuse("{s}: \"model_endpoint\" must be an object", .{where});
    const base_url = try reader.string(object, where, "base_url");
    if (!validModelEndpointUrl(base_url))
        return reader.refuse("{s}: model_endpoint base_url must be an absolute HTTPS URL or loopback HTTP URL without userinfo, query, or fragment", .{where});
    if (hermes and !try hermesKeepsResponses(reader.arena, base_url))
        return reader.refuse("{s}: Hermes reclassifies this model_endpoint base_url away from openai-responses", .{where});
    const wire = try reader.string(object, where, "wire");
    if (pi or opencode) {
        if (!std.mem.eql(u8, wire, "anthropic-messages") and !std.mem.eql(u8, wire, "openai-completions") and !std.mem.eql(u8, wire, "openai-responses"))
            return reader.refuse("{s}: model_endpoint wire must be anthropic-messages, openai-completions, or openai-responses", .{where});
    } else if (object.contains("wire") and !std.mem.eql(u8, wire, fixed_wire))
        return reader.refuse("{s}: {s} model_endpoint requires the {s} wire", .{ where, kind, fixed_wire });
    if (object.contains("auth") and !claude)
        return reader.refuse("{s}: model_endpoint auth is only supported on claude adapters", .{where});
    const auth = try reader.string(object, where, "auth");
    const bearer = std.mem.eql(u8, auth, "bearer");
    if (object.contains("auth") and !bearer and !std.mem.eql(u8, auth, "api-key"))
        return reader.refuse("{s}: model_endpoint auth must be api-key or bearer", .{where});
    const variable = try reader.string(object, where, "credential_env");
    if (variable.len == 0)
        return reader.refuse("{s}: model_endpoint credential_env must name a variable", .{where});
    const env_names = std.process.Environ.Map.EnvNameHashContext{};
    if (pi or opencode) {
        if (pi and !std.ascii.isAlphabetic(variable[0]) and variable[0] != '_')
            return reader.refuse("{s}: model_endpoint credential_env must be an interpolatable variable name", .{where});
        for (variable) |byte| {
            if (!std.ascii.isAlphanumeric(byte) and byte != '_')
                return reader.refuse("{s}: model_endpoint credential_env must be an interpolatable variable name", .{where});
        }
        for (destinations) |destination| {
            if (env_names.eql(variable, destination, 0))
                return reader.refuse("{s}: model_endpoint credential_env conflicts with {s}", .{ where, destination });
        }
        if (opencode and env_names.eql(variable, "OPENCODE_PASSWORD", 0))
            return reader.refuse("{s}: model_endpoint credential_env conflicts with OPENCODE_PASSWORD", .{where});
    }
    for (environment) |item| {
        const name = item[0 .. std.mem.indexOfScalar(u8, item, '=') orelse item.len];
        if (env_names.eql(name, variable, 0))
            return reader.refuse("{s}: \"environment\" names {s}, which model_endpoint controls", .{ where, name });
        for (destinations) |destination| {
            if (env_names.eql(name, destination, 0))
                return reader.refuse("{s}: \"environment\" names {s}, which model_endpoint controls", .{ where, name });
        }
        if (hermes and hermesNumberedKey(name))
            return reader.refuse("{s}: \"environment\" names {s}, which can override the model_endpoint credential", .{ where, name });
    }
    const key: []const u8 = environ.get(variable) orelse "";
    if (key.len == 0)
        return reader.refuse("{s}: model_endpoint credential variable {s} is unset or empty", .{ where, variable });
    const minimum_key_bytes = 16;
    if (key.len < minimum_key_bytes)
        return reader.refuse("{s}: model_endpoint credential variable {s} must contain at least {d} bytes for redaction", .{ where, variable, minimum_key_bytes });
    return .{ .base_url = base_url, .credential_env = variable, .bearer = bearer, .wire = wire };
}

pub const claude_endpoint_variables = [_][]const u8{ "ANTHROPIC_BASE_URL", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN" };
pub const codex_endpoint_variable = "OAPX_PROVIDER_API_KEY";
pub const deepseek_endpoint_variables = [_][]const u8{ "DEEPSEEK_BASE_URL", "DEEPSEEK_API_KEY" };
pub const hermes_endpoint_variables = [_][]const u8{ "OPENAI_BASE_URL", "OPENAI_API_KEY", "HERMES_HOME" };
pub const opencode_endpoint_variables = [_][]const u8{ "OPENCODE_CONFIG", "OPENCODE_CONFIG_CONTENT" };
pub const pi_endpoint_variable = "PI_CODING_AGENT_DIR";

fn hermesModelBlank(model: []const u8) bool {
    const view = std.unicode.Utf8View.init(model) catch return false;
    var chars = view.iterator();
    while (chars.nextCodepoint()) |c| switch (c) {
        0x09...0x0d, 0x1c...0x20, 0x85, 0xa0, 0x1680, 0x2000...0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000 => {},
        else => return false,
    };
    return true;
}

fn hermesNumberedKey(name: []const u8) bool {
    const prefix = "OPENAI_API_KEY_";
    if (name.len <= prefix.len) return false;
    const starts_with = if (@import("builtin").os.tag == .windows)
        std.ascii.eqlIgnoreCase(name[0..prefix.len], prefix)
    else
        std.mem.startsWith(u8, name, prefix);
    if (!starts_with) return false;
    const suffix = name[prefix.len..];
    if (suffix[0] == '0' or (suffix.len == 1 and suffix[0] == '1')) return false;
    for (suffix) |digit| {
        if (!std.ascii.isDigit(digit)) return false;
    }
    return true;
}

fn hermesKeepsResponses(arena: std.mem.Allocator, url: []const u8) !bool {
    const lower = try arena.dupe(u8, url);
    for (lower) |*byte| byte.* = std.ascii.toLower(byte.*);
    const uri = std.Uri.parse(lower) catch return false;
    const host = std.mem.trimEnd(u8, uri.host.?.percent_encoded, ".");
    for ([_][]const u8{ "api.anthropic.com", "api.actual.inc" }) |other| {
        if (std.mem.eql(u8, host, other)) return false;
    }
    for ([_][]const u8{ "api.x.ai", "api.meta.ai", "api.router.com", "api.openai.com" }) |responses| {
        if (std.mem.eql(u8, host, responses)) return true;
    }
    if (std.mem.endsWith(u8, host, ".api.openai.com")) return true;
    if (std.mem.startsWith(u8, host, "bedrock-runtime.") and std.mem.endsWith(u8, host, ".amazonaws.com")) return false;
    const path = std.mem.trimEnd(u8, uri.path.percent_encoded, "/");
    if (std.mem.eql(u8, host, "api.kimi.com") and std.mem.indexOf(u8, path, "/coding") != null) return false;
    const last_segment = if (std.mem.lastIndexOfScalar(u8, path, '/')) |index| index + 1 else 0;
    const params = std.mem.indexOfScalarPos(u8, path, last_segment, ';') orelse path.len;
    return !std.mem.endsWith(u8, path[0..params], "/anthropic") and
        !std.mem.endsWith(u8, path[0..params], "/anthropic/v1");
}

fn validModelEndpointUrl(url: []const u8) bool {
    for (url, 0..) |byte, i| {
        if (byte <= ' ' or byte >= 127 or std.mem.indexOfScalar(u8, "\\<>\"{}|^`", byte) != null) return false;
        if (byte == '%' and (url.len - i < 3 or !std.ascii.isHex(url[i + 1]) or !std.ascii.isHex(url[i + 2]))) return false;
    }
    const uri = std.Uri.parse(url) catch return false;
    if (uri.user != null or uri.password != null or uri.query != null or uri.fragment != null) return false;
    const host = (uri.host orelse return false).percent_encoded;
    const after_host = url[uri.scheme.len + "://".len + host.len ..];
    const port = after_host[0 .. std.mem.indexOfScalar(u8, after_host, '/') orelse after_host.len];
    if (port.len > 0) {
        if (port[0] != ':' or port.len == 1) return false;
        for (port[1..]) |digit| if (!std.ascii.isDigit(digit)) return false;
    }
    if (std.mem.startsWith(u8, host, "[") and std.mem.endsWith(u8, host, "]")) {
        const ip = std.Io.net.IpAddress.parseIp6(host[1 .. host.len - 1], 0) catch return false;
        return std.ascii.eqlIgnoreCase(uri.scheme, "https") or
            (std.ascii.eqlIgnoreCase(uri.scheme, "http") and std.mem.eql(u8, &ip.ip6.bytes, &std.Io.net.Ip6Address.loopback(0).bytes));
    }
    _ = std.Io.net.HostName.init(host) catch return false;
    if (std.ascii.eqlIgnoreCase(uri.scheme, "https")) return true;
    if (!std.ascii.eqlIgnoreCase(uri.scheme, "http")) return false;
    if (std.ascii.eqlIgnoreCase(host, "localhost") or std.ascii.eqlIgnoreCase(host, "localhost.")) return true;
    const ip = std.Io.net.IpAddress.parseIp4(host, 0) catch return false;
    return ip.ip4.bytes[0] == 127;
}

fn readToolSource(reader: Reader, id: []const u8, value: std.json.Value, environ: *const std.process.Environ.Map) Error!ToolSource {
    const where = try std.fmt.allocPrint(reader.arena, "tool source \"{s}\"", .{id});
    const object = try reader.members(value, where, &source_members) orelse return reader.refuse("{s}: kind is required", .{where});
    const kind = try reader.string(object, where, "kind");
    if (kind.len == 0) return reader.refuse("{s}: kind is required", .{where});
    if (!listed(&source_kinds, kind)) return reader.refuse("{s}: kind \"{s}\" is not a tool source kind", .{ where, kind });
    const command = try reader.string(object, where, "command");
    if (std.mem.eql(u8, kind, "process") and command.len == 0) return reader.refuse("{s}: a process source needs a command", .{where});
    const declared = try reader.strings(object, where, "environment") orelse &.{};
    if (duplicateName(declared)) |twice| return reader.refuse("{s}: environment names \"{s}\" twice", .{ where, twice });
    const environment = try resolveEnvironment(reader, where, declared, environ, true);
    const args = try reader.strings(object, where, "args") orelse &.{};
    var headers: std.ArrayList(std.http.Header) = .empty;
    if (object.get("headers")) |raw| if (raw != .null) {
        if (!std.mem.eql(u8, kind, "remote") or raw != .object) return reader.refuse("{s}: headers require a remote source and an object", .{where});
        var entries = raw.object.iterator();
        while (entries.next()) |entry| {
            const name = entry.key_ptr.*;
            if (!validHeaderName(name) or reservedHeader(name)) return reader.refuse("{s}: invalid or reserved header name", .{where});
            for (headers.items) |previous| if (std.ascii.eqlIgnoreCase(previous.name, name)) return reader.refuse("{s}: duplicate header name", .{where});
            const resolved = try headerValue(reader, where, entry.value_ptr.*, environ);
            if (!validHeaderValue(resolved)) return reader.refuse("{s}: invalid header value", .{where});
            try headers.append(reader.arena, .{ .name = name, .value = resolved });
        }
    };
    if (object.get("auth")) |raw| if (reader.service_config or raw != .null) {
        if (!std.mem.eql(u8, kind, "remote")) return reader.refuse("{s}: auth requires a remote source", .{where});
        for (headers.items) |header| if (std.ascii.eqlIgnoreCase(header.name, "authorization")) return reader.refuse("{s}: auth conflicts with the authorization header", .{where});
        if (reader.service_config and raw != .object)
            return reader.refuse("{s}: auth must name an environment variable using {{env: NAME}}", .{where});
        const bearer = try headerValue(reader, where, raw, environ);
        if (!validBearer(bearer)) return reader.refuse("{s}: invalid bearer token", .{where});
        try headers.append(reader.arena, .{ .name = "Authorization", .value = try std.fmt.allocPrint(reader.arena, "Bearer {s}", .{bearer}) });
    };
    return .{
        .id = id,
        .kind = kind,
        .display_name = try reader.string(object, where, "display_name"),
        .protocol = try reader.string(object, where, "protocol"),
        .endpoint = try reader.string(object, where, "endpoint"),
        .command = command,
        .args = args,
        .environment = environment,
        .headers = headers.items,
    };
}

fn headerValue(reader: Reader, where: []const u8, value: std.json.Value, environ: *const std.process.Environ.Map) Error![]const u8 {
    if (value == .string) return value.string;
    if (value != .object or value.object.count() != 1) return reader.refuse("{s}: header value must be a string or an env reference", .{where});
    const field = value.object.get("env") orelse return reader.refuse("{s}: header value must be a string or an env reference", .{where});
    if (field != .string or field.string.len == 0) return reader.refuse("{s}: env reference needs a variable name", .{where});
    const resolved = environ.get(field.string) orelse return reader.refuse("{s}: environment variable \"{s}\" is not set", .{ where, field.string });
    if (resolved.len == 0) return reader.refuse("{s}: environment variable \"{s}\" is empty", .{ where, field.string });
    return resolved;
}

fn validHeaderName(name: []const u8) bool {
    if (name.len == 0) return false;
    for (name) |c| if (!std.ascii.isAlphanumeric(c) and std.mem.indexOfScalar(u8, "!#$%&'*+-.^_`|~", c) == null) return false;
    return true;
}

fn reservedHeader(name: []const u8) bool {
    for ([_][]const u8{ "host", "content-length", "transfer-encoding", "connection", "accept", "content-type", "mcp-session-id", "mcp-protocol-version" }) |reserved| {
        if (std.ascii.eqlIgnoreCase(name, reserved)) return true;
    }
    return false;
}

fn validHeaderValue(value: []const u8) bool {
    for (value) |c| if ((c < 32 and c != '\t') or c == 127) return false;
    return true;
}

fn validBearer(value: []const u8) bool {
    if (value.len == 0 or value[0] == '=') return false;
    var padding = false;
    for (value) |c| {
        if (c == '=') padding = true else if (padding or !(std.ascii.isAlphanumeric(c) or std.mem.indexOfScalar(u8, "-._~+/", c) != null)) return false;
    }
    return true;
}

fn resolveEnvironment(reader: Reader, where: []const u8, entries: []const []const u8, environ: *const std.process.Environ.Map, required: bool) Error![]const []const u8 {
    var resolved = std.ArrayList([]const u8).empty;
    try resolved.ensureTotalCapacity(reader.arena, entries.len);
    for (entries) |entry| {
        const split = std.mem.indexOfScalar(u8, entry, '=');
        const name = if (split) |at| entry[0..at] else entry;
        if (name.len == 0) return reader.refuse("{s}: environment entry has no variable name", .{where});
        if (split != null) {
            resolved.appendAssumeCapacity(entry);
            continue;
        }
        const value = environ.get(name) orelse {
            if (required) return reader.refuse("{s}: environment variable \"{s}\" is not set; export it or write {s}=<value>", .{ where, name, name });
            continue;
        };
        resolved.appendAssumeCapacity(try std.fmt.allocPrint(reader.arena, "{s}={s}", .{ name, value }));
    }
    return resolved.items;
}

pub fn toolPosture(arena: std.mem.Allocator, entry: AdapterEntry, diagnostic: *Diagnostic) Error!Posture {
    const reader = Reader{ .arena = arena, .diagnostic = diagnostic };
    if (entry.unrestricted_tools and entry.allowed_tools != null and entry.allowed_tools.?.len > 0) {
        return reader.refuse("adapter \"{s}\": set either \"allowed_tools\" or \"unrestricted_tools\", not both", .{entry.name});
    }
    if (entry.unrestricted_tools) return .unrestricted;
    const allowed = entry.allowed_tools orelse
        return reader.refuse("adapter \"{s}\": state its tool posture: set \"allowed_tools\" to the tools the child may use, or \"unrestricted_tools\": true to give it the harness default", .{entry.name});
    if (allowed.len == 0) {
        return reader.refuse("adapter \"{s}\": \"allowed_tools\" names no tool; name at least one, or set \"unrestricted_tools\": true to give it the harness default", .{entry.name});
    }
    for (allowed) |tool| {
        if (tool.len == 0) return reader.refuse("adapter \"{s}\": \"allowed_tools\" names an empty tool", .{entry.name});
    }
    return .{ .allowed = allowed };
}

pub const builtin_environment = [_][]const u8{ "HOME", "PATH" };

pub fn builtinClaude(arena: std.mem.Allocator, environ: *const std.process.Environ.Map) Error!AdapterEntry {
    var diagnostic = Diagnostic{};
    const reader = Reader{ .arena = arena, .diagnostic = &diagnostic };
    const environment = try resolveEnvironment(reader, "the built-in claude entry", &builtin_environment, environ, false);
    return .{ .name = "claude", .kind = "claude", .environment = environment, .unrestricted_tools = true };
}

pub fn builtinPi(arena: std.mem.Allocator, environ: *const std.process.Environ.Map) Error!AdapterEntry {
    var diagnostic = Diagnostic{};
    const reader = Reader{ .arena = arena, .diagnostic = &diagnostic };
    const environment = try resolveEnvironment(reader, "the built-in pi entry", &builtin_environment, environ, false);
    return .{ .name = "pi", .kind = "pi", .environment = environment };
}

pub fn builtinCodex(arena: std.mem.Allocator, environ: *const std.process.Environ.Map) Error!AdapterEntry {
    var diagnostic = Diagnostic{};
    const reader = Reader{ .arena = arena, .diagnostic = &diagnostic };
    const environment = try resolveEnvironment(reader, "the built-in codex entry", &builtin_environment, environ, false);
    return .{ .name = "codex", .kind = "codex", .environment = environment };
}

pub fn builtinOpencode(arena: std.mem.Allocator, environ: *const std.process.Environ.Map) Error!AdapterEntry {
    var diagnostic = Diagnostic{};
    const reader = Reader{ .arena = arena, .diagnostic = &diagnostic };
    const environment = try resolveEnvironment(reader, "the built-in opencode entry", &builtin_environment, environ, false);
    return .{ .name = "opencode", .kind = "opencode", .environment = environment };
}

pub fn resolveExecutable(arena: std.mem.Allocator, io: std.Io, name: []const u8, search_path: []const u8) Error!?[]const u8 {
    if (name.len == 0) return null;
    if (std.mem.indexOfScalar(u8, name, '/') != null) return name;
    var directories = std.mem.splitScalar(u8, search_path, ':');
    while (directories.next()) |directory| {
        if (!std.fs.path.isAbsolute(directory)) continue;
        const candidate = try std.fs.path.join(arena, &.{ directory, name });
        std.Io.Dir.accessAbsolute(io, candidate, .{ .execute = true }) catch continue;
        return candidate;
    }
    return null;
}

fn sortedKeys(arena: std.mem.Allocator, object: std.json.ObjectMap) Error![]const []const u8 {
    const keys = try arena.dupe([]const u8, object.keys());
    std.mem.sort([]const u8, keys, {}, lessThan);
    return keys;
}

fn lessThan(_: void, a: []const u8, b: []const u8) bool {
    return std.mem.order(u8, a, b) == .lt;
}

fn listed(values: []const []const u8, wanted: []const u8) bool {
    for (values) |value| {
        if (std.mem.eql(u8, value, wanted)) return true;
    }
    return false;
}

fn nonEmpty(value: []const u8) ?[]const u8 {
    return if (value.len > 0) value else null;
}

fn duplicateName(entries: []const []const u8) ?[]const u8 {
    for (entries, 0..) |entry, index| {
        const name = entry[0 .. std.mem.indexOfScalar(u8, entry, '=') orelse entry.len];
        for (entries[0..index]) |earlier| {
            const seen = earlier[0 .. std.mem.indexOfScalar(u8, earlier, '=') orelse earlier.len];
            if (std.mem.eql(u8, seen, name)) return name;
        }
    }
    return null;
}

const testing = std.testing;

const example = @embedFile("example_registry");

fn parseWith(arena: *std.heap.ArenaAllocator, bytes: []const u8, names: []const [2][]const u8, diagnostic: *Diagnostic) Error!File {
    var environ = std.process.Environ.Map.init(arena.allocator());
    for (names) |pair| try environ.put(pair[0], pair[1]);
    return parse(arena.allocator(), bytes, &environ, diagnostic);
}

fn refusal(bytes: []const u8, names: []const [2][]const u8) ![]const u8 {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    errdefer arena.deinit();
    var diagnostic = Diagnostic{};
    if (parseWith(&arena, bytes, names, &diagnostic)) |_| {
        arena.deinit();
        return error.ConfigAccepted;
    } else |err| {
        if (err != error.ConfigInvalid) return err;
    }
    const kept = try testing.allocator.dupe(u8, diagnostic.message);
    arena.deinit();
    return kept;
}

fn expectRefused(bytes: []const u8, names: []const [2][]const u8, expected: []const u8) !void {
    const message = try refusal(bytes, names);
    defer testing.allocator.free(message);
    try testing.expectEqualStrings(expected, message);
}

test "MCP config refuses invalid header name" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"bad name\":\"x\"}}}}", &.{}, "config: tool source \"fs\": invalid or reserved header name");
}

test "MCP config refuses reserved header name" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"Host\":\"x\"}}}}", &.{}, "config: tool source \"fs\": invalid or reserved header name");
}

test "MCP config refuses duplicate header name" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"X-Foo\":\"x\",\"x-foo\":\"y\"}}}}", &.{}, "config: tool source \"fs\": duplicate header name");
}

test "MCP config refuses non-object headers" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":[]}}}", &.{}, "config: tool source \"fs\": headers require a remote source and an object");
}

test "MCP config refuses invalid header value shape" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"X-Foo\":42}}}}", &.{}, "config: tool source \"fs\": header value must be a string or an env reference");
}

test "MCP config refuses invalid env reference" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"X-Foo\":{\"env\":\"\"}}}}}", &.{}, "config: tool source \"fs\": env reference needs a variable name");
}

test "MCP config refuses missing env reference" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"X-Foo\":{\"env\":\"MISSING\"}}}}}", &.{}, "config: tool source \"fs\": environment variable \"MISSING\" is not set");
}

test "MCP config refuses empty env reference" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"X-Foo\":{\"env\":\"EMPTY\"}}}}}", &.{.{ "EMPTY", "" }}, "config: tool source \"fs\": environment variable \"EMPTY\" is empty");
}

test "MCP config refuses invalid header value" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"headers\":{\"X-Foo\":\"bad\\rvalue\"}}}}", &.{}, "config: tool source \"fs\": invalid header value");
}

test "MCP config refuses invalid bearer" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"auth\":\"bad token\"}}}", &.{}, "config: tool source \"fs\": invalid bearer token");
}

test "MCP config refuses conflicting auth" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"auth\":\"token\",\"headers\":{\"authorization\":\"other\"}}}}", &.{}, "config: tool source \"fs\": auth conflicts with the authorization header");
}

test "MCP config refuses headers on a non-remote source" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"native\",\"headers\":{}}}}", &.{}, "config: tool source \"fs\": headers require a remote source and an object");
}

test "MCP config refuses auth on a non-remote source" {
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"native\",\"auth\":\"token\"}}}", &.{}, "config: tool source \"fs\": auth requires a remote source");
}

test "the example registry parses, each entry typed and resolved as it is written" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var diagnostic = Diagnostic{};
    const file = try parseWith(&arena, example, &.{ .{ "ANTHROPIC_API_KEY", "key" }, .{ "MCP_TOKEN", "token" } }, &diagnostic);

    try testing.expectEqual(@as(usize, 8), file.adapters.len);
    try testing.expectEqualStrings("acp", file.adapters[0].name);
    const claude = file.adapter("claude").?;
    try testing.expectEqualStrings("claude", claude.kind);
    try testing.expectEqualStrings("/absolute/path/to/claude", claude.executable);
    try testing.expectEqualStrings("/absolute/project/path", claude.working_directory.?);
    try testing.expectEqualStrings("sonnet-5", claude.model);
    try testing.expectEqual(@as(usize, 1), claude.environment.len);
    try testing.expectEqualStrings("ANTHROPIC_API_KEY=key", claude.environment[0]);
    try testing.expectEqual(@as(usize, 3), claude.allowed_tools.?.len);
    try testing.expectEqualStrings("Glob", claude.allowed_tools.?[2]);
    try testing.expectEqual(@as(i64, 64), file.adapter("memory").?.journal_capacity.?);
    try testing.expectEqualStrings("http://127.0.0.1:4096", file.adapter("opencode").?.endpoint);
    const hermes = file.adapter("hermes").?;
    try testing.expectEqual(@as(usize, 2), hermes.args.len);
    try testing.expectEqualStrings("-m", hermes.args[0]);
    try testing.expectEqualStrings("tui_gateway.entry", hermes.args[1]);
    try testing.expect(file.adapter("absent") == null);

    try testing.expectEqual(@as(usize, 1), file.tool_sources.len);
    const filesystem = file.tool_sources[0];
    try testing.expectEqualStrings("filesystem", filesystem.id);
    try testing.expectEqualStrings("process", filesystem.kind);
    try testing.expectEqualStrings("/absolute/path/to/mcp-filesystem", filesystem.command);
    try testing.expectEqual(@as(usize, 2), filesystem.args.len);
    try testing.expectEqualStrings("MCP_TOKEN=token", filesystem.environment[0]);
}

test "an unknown field is refused wherever it appears, naming the field and where" {
    try expectRefused("{\"adapters\":{},\"daemon\":{}}", &.{}, "config: the file: unknown field \"daemon\"");
    try expectRefused("{\"adapters\":{\"claude\":{\"Executable\":\"/bin/claude\"}}}", &.{}, "config: adapter \"claude\": unknown field \"Executable\"");
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"native\",\"secret\":\"x\"}}}", &.{}, "config: tool source \"fs\": unknown field \"secret\"");
}

test "the first unknown field named is the first in sorted order, wherever the object is" {
    try expectRefused("{\"adapters\":{\"memory\":{\"Zz\":1,\"Aa\":2}}}", &.{}, "config: adapter \"memory\": unknown field \"Aa\"");
    try expectRefused("{\"adapters\":{},\"tool_sources\":{},\"daemon\":1,\"bus\":2}", &.{}, "config: the file: unknown field \"bus\"");
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"native\",\"zeta\":1,\"alpha\":2}}}", &.{}, "config: tool source \"fs\": unknown field \"alpha\"");
}

test "a member of the wrong JSON type is refused naming it" {
    try expectRefused("{\"adapters\":{\"claude\":{\"executable\":7}}}", &.{}, "config: adapter \"claude\": \"executable\" must be a string");
    try expectRefused("{\"adapters\":{\"claude\":{\"args\":[\"--x\",7]}}}", &.{}, "config: adapter \"claude\": \"args\" must be an array of strings");
    try expectRefused("{\"adapters\":{\"claude\":{\"args\":\"--x\"}}}", &.{}, "config: adapter \"claude\": \"args\" must be an array of strings");
    try expectRefused("{\"adapters\":{\"claude\":{\"unrestricted_tools\":\"yes\"}}}", &.{}, "config: adapter \"claude\": \"unrestricted_tools\" must be a boolean");
    try expectRefused("{\"adapters\":{\"memory\":{\"journal_capacity\":1.5}}}", &.{}, "config: adapter \"memory\": \"journal_capacity\" must be an integer");
    try expectRefused("{\"adapters\":{\"claude\":[]}}", &.{}, "config: adapter \"claude\": must be an object");
    try expectRefused("{\"adapters\":[]}", &.{}, "config: adapters: must be an object");
    try expectRefused("{\"tool_sources\":7}", &.{}, "config: tool_sources: must be an object");
}

test "the file must be one JSON object, once, with no key twice; a null file is empty" {
    const trailing = try refusal("{\"adapters\":{}} {}", &.{});
    defer testing.allocator.free(trailing);
    try testing.expectEqualStrings("config: not one JSON object: SyntaxError", trailing);
    const doubled = try refusal("{\"adapters\":{},\"adapters\":{}}", &.{});
    defer testing.allocator.free(doubled);
    try testing.expectEqualStrings("config: not one JSON object: DuplicateField", doubled);
    const twice = try refusal("{\"adapters\":{\"memory\":{\"type\":\"memory\",\"type\":\"memory\"}}}", &.{});
    defer testing.allocator.free(twice);
    try testing.expectEqualStrings("config: not one JSON object: DuplicateField", twice);
    try expectRefused("[]", &.{}, "config: the file: must be an object");

    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var diagnostic = Diagnostic{};
    const empty = try parseWith(&arena, "null", &.{}, &diagnostic);
    try testing.expectEqual(@as(usize, 0), empty.adapters.len);
}

test "an entry without a type is typed by its name, and a null member reads as absent" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var diagnostic = Diagnostic{};
    const file = try parseWith(&arena, "{\"adapters\":{\"claude\":{\"type\":null,\"executable\":null,\"allowed_tools\":null,\"agent_config\":null},\"work\":{\"type\":\"claude\",\"agent_config\":{\"b\":[1]}},\"bare\":null},\"tool_sources\":null}", &.{}, &diagnostic);
    try testing.expectEqual(@as(usize, 3), file.adapters.len);
    const claude = file.adapter("claude").?;
    try testing.expectEqualStrings("claude", claude.kind);
    try testing.expectEqualStrings("", claude.executable);
    try testing.expect(claude.allowed_tools == null);
    try testing.expect(claude.agent_config_json == null);
    try testing.expectEqualStrings("claude", file.adapter("work").?.kind);
    try testing.expectEqualStrings("{\"b\":[1]}", file.adapter("work").?.agent_config_json.?);
    try testing.expectEqualStrings("bare", file.adapter("bare").?.kind);
}

test "an adapter's environment passes literals, resolves listed names, and drops names the parent never set" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var diagnostic = Diagnostic{};
    const file = try parseWith(&arena, "{\"adapters\":{\"claude\":{\"environment\":[\"LITERAL=a=b\",\"HOME\",\"UNSET\",\"EMPTY=\"]}}}", &.{ .{ "HOME", "/home/me" }, .{ "PATH", "/bin" } }, &diagnostic);
    const environment = file.adapter("claude").?.environment;
    try testing.expectEqual(@as(usize, 3), environment.len);
    try testing.expectEqualStrings("LITERAL=a=b", environment[0]);
    try testing.expectEqualStrings("HOME=/home/me", environment[1]);
    try testing.expectEqualStrings("EMPTY=", environment[2]);

    try expectRefused("{\"adapters\":{\"claude\":{\"environment\":[\"=value\"]}}}", &.{}, "config: adapter \"claude\": environment entry has no variable name");
    try expectRefused("{\"adapters\":{\"claude\":{\"environment\":[\"\"]}}}", &.{}, "config: adapter \"claude\": environment entry has no variable name");
}

test "a tool source names a known kind, a command when it is a process, each variable once, and only variables that are set" {
    try expectRefused("{\"tool_sources\":{\"fs\":{}}}", &.{}, "config: tool source \"fs\": kind is required");
    try expectRefused("{\"tool_sources\":{\"fs\":null}}", &.{}, "config: tool source \"fs\": kind is required");
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"plugin\"}}}", &.{}, "config: tool source \"fs\": kind \"plugin\" is not a tool source kind");
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"process\"}}}", &.{}, "config: tool source \"fs\": a process source needs a command");
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"environment\":[\"TOKEN=a\",\"TOKEN\"]}}}", &.{.{ "TOKEN", "b" }}, "config: tool source \"fs\": environment names \"TOKEN\" twice");
    try expectRefused("{\"tool_sources\":{\"fs\":{\"kind\":\"remote\",\"environment\":[\"TOKEN\"]}}}", &.{}, "config: tool source \"fs\": environment variable \"TOKEN\" is not set; export it or write TOKEN=<value>");

    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var diagnostic = Diagnostic{};
    const file = try parseWith(&arena, "{\"tool_sources\":{\"b\":{\"kind\":\"native\"},\"a\":{\"kind\":\"hosted\",\"environment\":[\"TOKEN\"]}}}", &.{.{ "TOKEN", "t" }}, &diagnostic);
    try testing.expectEqualStrings("a", file.tool_sources[0].id);
    try testing.expectEqualStrings("TOKEN=t", file.tool_sources[0].environment[0]);
    try testing.expectEqualStrings("b", file.tool_sources[1].id);
}

test "a claude tool posture is stated exactly once, and names no empty tool" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var diagnostic = Diagnostic{};

    try testing.expectEqual(Posture.unrestricted, try toolPosture(arena.allocator(), .{ .name = "claude", .kind = "claude", .unrestricted_tools = true }, &diagnostic));
    const allowed = try toolPosture(arena.allocator(), .{ .name = "claude", .kind = "claude", .allowed_tools = &.{ "Read", "Grep" } }, &diagnostic);
    try testing.expectEqualStrings("Grep", allowed.allowed[1]);

    const cases = [_]struct { entry: AdapterEntry, message: []const u8 }{
        .{ .entry = .{ .name = "c", .kind = "claude" }, .message = "config: adapter \"c\": state its tool posture: set \"allowed_tools\" to the tools the child may use, or \"unrestricted_tools\": true to give it the harness default" },
        .{ .entry = .{ .name = "c", .kind = "claude", .unrestricted_tools = true, .allowed_tools = &.{"Read"} }, .message = "config: adapter \"c\": set either \"allowed_tools\" or \"unrestricted_tools\", not both" },
        .{ .entry = .{ .name = "c", .kind = "claude", .allowed_tools = &.{} }, .message = "config: adapter \"c\": \"allowed_tools\" names no tool; name at least one, or set \"unrestricted_tools\": true to give it the harness default" },
        .{ .entry = .{ .name = "c", .kind = "claude", .allowed_tools = &.{ "Read", "" } }, .message = "config: adapter \"c\": \"allowed_tools\" names an empty tool" },
    };
    for (cases) |case| {
        try testing.expectError(error.ConfigInvalid, toolPosture(arena.allocator(), case.entry, &diagnostic));
        try testing.expectEqualStrings(case.message, diagnostic.message);
    }
}

test "the built-in claude entry passes only HOME and PATH, and takes the harness-default tool posture" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var environ = std.process.Environ.Map.init(arena.allocator());
    try environ.put("HOME", "/home/me");
    try environ.put("PATH", "/usr/bin");
    try environ.put("ANTHROPIC_API_KEY", "secret");
    try environ.put("AWS_SECRET_ACCESS_KEY", "secret");

    const entry = try builtinClaude(arena.allocator(), &environ);
    try testing.expectEqualStrings("claude", entry.kind);
    try testing.expectEqualStrings("", entry.executable);
    try testing.expectEqual(@as(usize, 2), entry.environment.len);
    try testing.expectEqualStrings("HOME=/home/me", entry.environment[0]);
    try testing.expectEqualStrings("PATH=/usr/bin", entry.environment[1]);
    var diagnostic = Diagnostic{};
    try testing.expectEqual(Posture.unrestricted, try toolPosture(arena.allocator(), entry, &diagnostic));
}

test "the built-in codex entry passes only HOME and PATH and states no approval policy or sandbox" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var environ = std.process.Environ.Map.init(arena.allocator());
    try environ.put("HOME", "/home/me");
    try environ.put("PATH", "/usr/bin");
    try environ.put("OPENAI_API_KEY", "secret");

    const entry = try builtinCodex(arena.allocator(), &environ);
    try testing.expectEqualStrings("codex", entry.kind);
    try testing.expectEqualStrings("", entry.executable);
    try testing.expectEqual(@as(usize, 2), entry.environment.len);
    try testing.expectEqualStrings("HOME=/home/me", entry.environment[0]);
    try testing.expectEqualStrings("PATH=/usr/bin", entry.environment[1]);
    try testing.expectEqualStrings("", entry.approval_policy);
    try testing.expectEqualStrings("", entry.sandbox);
}

test "the built-in pi entry passes only HOME and PATH" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var environ = std.process.Environ.Map.init(arena.allocator());
    try environ.put("HOME", "/home/me");
    try environ.put("PATH", "/usr/bin");
    try environ.put("ANTHROPIC_API_KEY", "secret");

    const entry = try builtinPi(arena.allocator(), &environ);
    try testing.expectEqualStrings("pi", entry.kind);
    try testing.expectEqualStrings("", entry.executable);
    try testing.expectEqual(@as(usize, 2), entry.environment.len);
    try testing.expectEqualStrings("HOME=/home/me", entry.environment[0]);
    try testing.expectEqualStrings("PATH=/usr/bin", entry.environment[1]);
}

test "a bare executable is found on the search path's absolute directories, and a path is taken as written" {
    if (@import("builtin").os.tag == .windows) return error.SkipZigTest;
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const scratch = arena.allocator();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.writeFile(testing.io, .{ .sub_path = "claude", .data = "#!/bin/sh\n", .flags = .{ .permissions = .executable_file } });
    try tmp.dir.writeFile(testing.io, .{ .sub_path = "notes", .data = "text\n" });
    const cwd = try std.process.currentPathAlloc(testing.io, scratch);
    const directory = try std.fs.path.join(scratch, &.{ cwd, ".zig-cache", "tmp", tmp.sub_path[0..] });
    const search = try std.fmt.allocPrint(scratch, "relative/dir:/nonexistent:{s}", .{directory});

    const found = (try resolveExecutable(scratch, testing.io, "claude", search)).?;
    try testing.expectEqualStrings(try std.fs.path.join(scratch, &.{ directory, "claude" }), found);
    try testing.expect(try resolveExecutable(scratch, testing.io, "notes", search) == null);
    try testing.expect(try resolveExecutable(scratch, testing.io, "absent", search) == null);
    try testing.expect(try resolveExecutable(scratch, testing.io, "claude", "relative/dir") == null);
    try testing.expect(try resolveExecutable(scratch, testing.io, "", search) == null);
    try testing.expectEqualStrings("./bin/claude", (try resolveExecutable(scratch, testing.io, "./bin/claude", "")).?);
}

fn parseExample(allocator: std.mem.Allocator) !void {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    var environ = std.process.Environ.Map.init(allocator);
    defer environ.deinit();
    try environ.put("ANTHROPIC_API_KEY", "key");
    try environ.put("MCP_TOKEN", "token");
    var diagnostic = Diagnostic{};
    const file = try parse(arena.allocator(), example, &environ, &diagnostic);
    _ = try toolPosture(arena.allocator(), file.adapter("claude").?, &diagnostic);
}

test "parsing frees what it built when any allocation fails" {
    try testing.checkAllAllocationFailures(testing.allocator, parseExample, .{});
}

test "model_provider is read on claude, codex and deepseek adapters and refused by name on any other" {
    var arena_state = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena_state.deinit();
    const arena = arena_state.allocator();
    var environ = std.process.Environ.Map.init(arena);
    var diagnostic = Diagnostic{};
    const file = try parse(arena, "{\"adapters\":{\"glm\":{\"type\":\"claude\",\"model_provider\":\"minimax-coding-plan\"}}}", &environ, &diagnostic);
    try std.testing.expectEqualStrings("minimax-coding-plan", file.adapter("glm").?.model_provider);

    const routed = try parse(arena, "{\"adapters\":{\"codex\":{\"model_provider\":\"openai\"}}}", &environ, &diagnostic);
    try std.testing.expectEqualStrings("openai", routed.adapter("codex").?.model_provider);

    const deepseek = try parse(arena, "{\"adapters\":{\"ds\":{\"type\":\"deepseek\",\"model_provider\":\"opencode-go\"}}}", &environ, &diagnostic);
    try std.testing.expectEqualStrings("opencode-go", deepseek.adapter("ds").?.model_provider);

    try std.testing.expectError(error.ConfigInvalid, parse(arena, "{\"adapters\":{\"pi\":{\"model_provider\":\"deepseek\"}}}", &environ, &diagnostic));
    try std.testing.expectEqualStrings("config: adapter \"pi\": \"model_provider\" routes only claude, codex, and deepseek adapters; a pi adapter has no provider setting oxap translates it into", diagnostic.message);
}
