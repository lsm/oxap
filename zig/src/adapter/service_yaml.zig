const std = @import("std");

const Value = std.json.Value;
const Allocator = std.mem.Allocator;
pub const Error = Allocator.Error || error{InvalidYaml};
const max_depth = 64;

pub fn parse(arena: Allocator, input: []const u8, diagnostic: *[]const u8) Error!Value {
    var parser = Parser{ .arena = arena, .input = input, .diagnostic = diagnostic };
    if (!std.unicode.utf8ValidateSlice(input)) return parser.fail("invalid UTF-8");
    while (parser.pos < input.len) : (parser.pos += 1) {
        const byte = input[parser.pos];
        if ((byte < 0x20 and byte != '\n' and byte != '\r' and byte != '\t') or byte == 0x7f)
            return parser.fail("control character");
        if (byte == '\r' and parser.next() != '\n') return parser.fail("expected LF after CR");
    }
    parser.pos = 0;
    const trimmed = std.mem.trimStart(u8, input, " \t\r\n");
    if (trimmed.len > 0 and (trimmed[0] == '{' or trimmed[0] == '[')) {
        if (std.json.parseFromSliceLeaky(Value, arena, input, .{})) |value| {
            if (tooDeep(value, 0)) return parser.fail("nesting limit exceeded");
            return value;
        } else |err| if (err == error.OutOfMemory) return error.OutOfMemory;
    }
    try parser.prepare();
    if (parser.marker("---")) try parser.endLineAfterMarker();
    if (parser.peek() == 0) return .null;
    if (parser.column() != 0) return parser.fail("expected an unindented document");
    const value = try parser.block(0, 0);
    if (parser.marker("...")) try parser.endLineAfterMarker();
    if (parser.peek() != 0) return parser.fail("unexpected indentation or another document");
    return value;
}

fn tooDeep(value: Value, depth: usize) bool {
    if (depth >= max_depth) return true;
    switch (value) {
        .array => |array| for (array.items) |child| {
            if (tooDeep(child, depth + 1)) return true;
        },
        .object => |object| for (object.values()) |child| {
            if (tooDeep(child, depth + 1)) return true;
        },
        else => {},
    }
    return false;
}

const Context = enum { block, flow };

const Parser = struct {
    arena: Allocator,
    input: []const u8,
    diagnostic: *[]const u8,
    pos: usize = 0,

    fn fail(self: Parser, message: []const u8) Error {
        const line = std.mem.count(u8, self.input[0..self.pos], "\n") + 1;
        self.diagnostic.* = try std.fmt.allocPrint(self.arena, "line {d}, column {d}: {s}", .{ line, self.column() + 1, message });
        return error.InvalidYaml;
    }

    fn column(self: Parser) usize {
        return self.pos - if (std.mem.lastIndexOfScalar(u8, self.input[0..self.pos], '\n')) |start| start + 1 else 0;
    }

    fn peek(self: Parser) u8 {
        return self.at(self.pos);
    }

    fn next(self: Parser) u8 {
        return self.at(self.pos + 1);
    }

    fn at(self: Parser, position: usize) u8 {
        return if (position < self.input.len) self.input[position] else 0;
    }

    fn take(self: *Parser, byte: u8) bool {
        if (self.peek() != byte) return false;
        self.pos += 1;
        return true;
    }

    fn horizontal(self: *Parser) void {
        while (self.peek() == ' ' or self.peek() == '\t') self.pos += 1;
    }

    fn comment(self: *Parser) void {
        while (!lineBreak(self.peek())) self.pos += 1;
    }

    fn prepare(self: *Parser) Error!void {
        while (true) {
            while (self.take(' ')) {}
            if (self.peek() == '\t') return self.fail("tabs cannot indent a block");
            if (self.peek() == '#') self.comment();
            _ = self.take('\r');
            if (!self.take('\n')) return;
        }
    }

    fn restOfLine(self: *Parser) Error!void {
        const previous = self.pos;
        self.horizontal();
        if (self.pos != previous and self.peek() == '#') self.comment();
        if (!lineBreak(self.peek())) return self.fail("expected end of line");
        _ = self.take('\r');
        _ = self.take('\n');
        try self.prepare();
    }

    fn flowSpace(self: *Parser) void {
        while (true) {
            self.horizontal();
            if (self.peek() == '#') self.comment();
            _ = self.take('\r');
            if (!self.take('\n')) return;
        }
    }

    fn marker(self: Parser, text: []const u8) bool {
        return self.column() == 0 and std.mem.startsWith(u8, self.input[self.pos..], text) and
            separation(self.at(self.pos + text.len));
    }

    fn endLineAfterMarker(self: *Parser) Error!void {
        self.pos += 3;
        try self.restOfLine();
    }

    fn block(self: *Parser, indent: usize, depth: usize) Error!Value {
        if (depth >= max_depth) return self.fail("nesting limit exceeded");
        if (self.marker("---") or self.marker("...")) return self.fail("only one document is allowed");
        if (self.sequenceItem()) return self.blockSequence(indent, depth);
        if (self.pairAhead()) return self.blockMapping(indent, depth);
        const value = try self.flowValue(.block, depth);
        try self.restOfLine();
        return value;
    }

    fn sequenceItem(self: Parser) bool {
        return self.peek() == '-' and separation(self.next());
    }

    fn pairAhead(self: Parser) bool {
        var index = self.pos;
        const first = self.at(index);
        if (first == '{' or first == '[') return false;
        if (first == '"' or first == '\'') {
            index += 1;
            while (!lineBreak(self.at(index))) : (index += 1) {
                const byte = self.at(index);
                if (first == '"' and byte == '\\' and !lineBreak(self.at(index + 1))) {
                    index += 1;
                } else if (byte == first) {
                    if (first == '\'' and self.at(index + 1) == '\'') {
                        index += 1;
                    } else {
                        index += 1;
                        while (self.at(index) == ' ' or self.at(index) == '\t') index += 1;
                        return self.at(index) == ':' and separation(self.at(index + 1));
                    }
                }
            }
            return false;
        }
        while (!lineBreak(self.at(index))) : (index += 1) {
            if (self.at(index) == ':' and separation(self.at(index + 1))) return true;
            if (self.at(index) == '#' and (index == self.pos or separation(self.at(index - 1)))) return false;
        }
        return false;
    }

    fn blockMapping(self: *Parser, indent: usize, depth: usize) Error!Value {
        var mapping: std.json.ObjectMap = .{};
        while (self.peek() != 0 and self.column() == indent and self.pairAhead()) {
            const name = try self.key(.block);
            try self.colon();
            const value = try self.blockChild(indent, depth + 1, false);
            try self.put(&mapping, name, value);
        }
        return .{ .object = mapping };
    }

    fn blockSequence(self: *Parser, indent: usize, depth: usize) Error!Value {
        var sequence: std.json.Array = .init(self.arena);
        while (self.column() == indent and self.sequenceItem()) {
            self.pos += 1;
            try sequence.append(try self.blockChild(indent, depth + 1, true));
        }
        return .{ .array = sequence };
    }

    fn blockChild(self: *Parser, parent: usize, depth: usize, compact: bool) Error!Value {
        self.horizontal();
        if (lineBreak(self.peek()) or self.peek() == '#') {
            if (self.peek() == '#') self.comment();
            try self.restOfLine();
            if (self.peek() == 0 or self.column() <= parent) return .null;
            return self.block(self.column(), depth);
        }
        if (compact and (self.sequenceItem() or self.pairAhead())) return self.block(self.column(), depth);
        const value = try self.flowValue(.block, depth);
        try self.restOfLine();
        return value;
    }

    fn flowValue(self: *Parser, context: Context, depth: usize) Error!Value {
        if (depth >= max_depth) return self.fail("nesting limit exceeded");
        return switch (self.peek()) {
            '{' => self.flowMapping(depth + 1),
            '[' => self.flowSequence(depth + 1),
            '\'', '"' => .{ .string = try self.quoted() },
            else => self.scalar(try self.plain(context, false)),
        };
    }

    fn flowMapping(self: *Parser, depth: usize) Error!Value {
        self.pos += 1;
        self.flowSpace();
        var mapping: std.json.ObjectMap = .{};
        while (!self.take('}')) {
            const name = try self.key(.flow);
            if (name.plain and !separation(self.next()) and self.next() != '[' and self.next() != '{')
                return self.fail("expected separation after a plain mapping key");
            try self.colon();
            self.flowSpace();
            const value: Value = if (self.peek() == ',' or self.peek() == '}') .null else try self.flowValue(.flow, depth);
            try self.put(&mapping, name, value);
            self.flowSpace();
            if (self.take('}')) break;
            if (!self.take(',')) return self.fail("expected ',' or '}'");
            self.flowSpace();
        }
        return .{ .object = mapping };
    }

    fn flowSequence(self: *Parser, depth: usize) Error!Value {
        self.pos += 1;
        self.flowSpace();
        var sequence: std.json.Array = .init(self.arena);
        while (!self.take(']')) {
            try sequence.append(try self.flowValue(.flow, depth));
            self.flowSpace();
            if (self.take(']')) break;
            if (!self.take(',')) return self.fail("expected ',' or ']'");
            self.flowSpace();
        }
        return .{ .array = sequence };
    }

    const Key = struct { text: []const u8, position: usize, plain: bool };

    fn key(self: *Parser, context: Context) Error!Key {
        const position = self.pos;
        const is_quoted = self.peek() == '"' or self.peek() == '\'';
        const text = if (is_quoted) try self.quoted() else try self.plain(context, true);
        if (!is_quoted and std.mem.eql(u8, text, "<<")) return self.fail("merge keys are unsupported");
        if (!is_quoted and (try self.scalar(text)) != .string)
            return self.fail("mapping keys must be strings");
        self.horizontal();
        return .{ .text = text, .position = position, .plain = !is_quoted };
    }

    fn colon(self: *Parser) Error!void {
        if (!self.take(':')) return self.fail("expected ':' after mapping key");
    }

    fn put(self: *Parser, mapping: *std.json.ObjectMap, name: Key, value: Value) Error!void {
        const slot = try mapping.getOrPut(self.arena, name.text);
        if (slot.found_existing) {
            self.pos = name.position;
            return self.fail("duplicate mapping key");
        }
        slot.value_ptr.* = value;
    }

    fn quoted(self: *Parser) Error![]const u8 {
        const delimiter = self.peek();
        const start = self.pos;
        self.pos += 1;
        var single: std.ArrayList(u8) = .empty;
        while (!lineBreak(self.peek())) {
            const byte = self.peek();
            self.pos += 1;
            if (byte == delimiter) {
                if (delimiter == '"') {
                    return std.json.parseFromSliceLeaky([]const u8, self.arena, self.input[start..self.pos], .{}) catch |err| {
                        if (err == error.OutOfMemory) return error.OutOfMemory;
                        return self.fail("invalid double-quoted string");
                    };
                }
                if (!self.take('\'')) return single.items;
            }
            if (delimiter == '\'') try single.append(self.arena, byte);
            if (delimiter == '"' and byte == '\\' and !lineBreak(self.peek())) self.pos += 1;
        }
        return self.fail("unterminated quoted string");
    }

    fn plain(self: *Parser, context: Context, mapping_key: bool) Error![]const u8 {
        const start = self.pos;
        switch (self.peek()) {
            '&', '*', '!', '|', '>', '%', '@', 0x60, '?', '{', '}', '[', ']', ',' => return self.fail("unsupported YAML indicator"),
            else => {},
        }
        while (!lineBreak(self.peek())) : (self.pos += 1) {
            const byte = self.peek();
            if (byte == '#' and (self.pos == start or separation(self.at(self.pos - 1)))) break;
            if (context == .flow and (byte == ',' or byte == ']' or byte == '}')) break;
            if (context == .flow and (byte == '[' or byte == '{')) return self.fail("unexpected flow collection");
            if (byte == ':' and separation(self.next())) break;
            if (mapping_key and context == .flow and byte == ':' and (self.next() == '[' or self.next() == '{')) break;
        }
        const text = std.mem.trimEnd(u8, self.input[start..self.pos], " \t");
        if (text.len == 0) return self.fail("expected a scalar");
        self.pos = start + text.len;
        return text;
    }

    fn scalar(self: Parser, text: []const u8) Error!Value {
        if (literal(text, "null", "Null", "NULL") or std.mem.eql(u8, text, "~")) return .null;
        if (literal(text, "true", "True", "TRUE")) return .{ .bool = true };
        if (literal(text, "false", "False", "FALSE")) return .{ .bool = false };
        const digits = if (text[0] == '+' or text[0] == '-') text[1..] else text;
        if (digits.len != 0 and for (digits) |digit| {
            if (!std.ascii.isDigit(digit)) break false;
        } else true) {
            const value = std.fmt.parseInt(i64, text, 10) catch
                return self.fail("integer is outside the signed 64-bit range");
            return .{ .integer = value };
        }
        return .{ .string = text };
    }
};

fn lineBreak(byte: u8) bool {
    return byte == 0 or byte == '\n' or byte == '\r';
}

fn separation(byte: u8) bool {
    return lineBreak(byte) or byte == ' ' or byte == '\t';
}

fn literal(text: []const u8, lower: []const u8, title: []const u8, upper: []const u8) bool {
    return std.mem.eql(u8, text, lower) or std.mem.eql(u8, text, title) or std.mem.eql(u8, text, upper);
}
