package harness

import (
	"io/fs"
	"regexp"
	"strconv"
	"strings"
	"unicode/utf8"
)

var rustTokenStart = regexp.MustCompile(`//|/\*|'(?:[^'\\\n]|\\(?:[^\n]|x[[:xdigit:]]{2}|u\{[[:xdigit:]_]+\}))'|r#*"|"`)
var rustBlockMark = regexp.MustCompile(`/\*|\*/`)

func rustLiterals(tree fs.FS, name string) []sourceLiteral {
	content, err := fs.ReadFile(tree, name)
	if err != nil {
		return nil
	}
	source := strings.ReplaceAll(string(content), "\r\n", "\n")
	line := 1
	var literals []sourceLiteral
	for {
		token := rustTokenStart.FindStringIndex(source)
		if token == nil {
			return literals
		}
		line += strings.Count(source[:token[0]], "\n")
		source = source[token[0]:]
		start := source[:token[1]-token[0]]
		end := len(start)
		switch start[0] {
		case '/':
			if start == "//" {
				end = strings.IndexByte(source, '\n')
				if end < 0 {
					return literals
				}
			} else {
				end = rustBlockEnd(source)
			}
		case '"':
			var value string
			value, end = rustQuoted(source)
			literals = append(literals, sourceLiteral{text: value, line: line})
		case 'r':
			close := "\"" + start[1:len(start)-1]
			offset := strings.Index(source[end:], close)
			if offset < 0 {
				return literals
			}
			literals = append(literals, sourceLiteral{text: source[end : end+offset], line: line})
			end += offset + len(close)
		}
		line += strings.Count(source[:end], "\n")
		source = source[end:]
	}
}

func rustBlockEnd(source string) int {
	end, depth := 2, 1
	for depth > 0 {
		mark := rustBlockMark.FindStringIndex(source[end:])
		if mark == nil {
			return len(source)
		}
		if source[end+mark[0]] == '/' {
			depth++
		} else {
			depth--
		}
		end += mark[1]
	}
	return end
}

func rustQuoted(source string) (string, int) {
	var value strings.Builder
	for offset := 1; offset < len(source); {
		next := strings.IndexAny(source[offset:], "\"\\")
		if next < 0 {
			break
		}
		value.WriteString(source[offset : offset+next])
		offset += next
		if source[offset] == '"' {
			return value.String(), offset + 1
		}
		decoded, consumed := rustEscape(source[offset:])
		if consumed == 0 {
			break
		}
		value.WriteString(decoded)
		offset += consumed
	}
	return "", len(source)
}

func rustEscape(source string) (string, int) {
	if len(source) < 2 {
		return "", 0
	}
	switch source[1] {
	case '\n':
		return "", len(source) - len(strings.TrimLeft(source[2:], " \t\r\n"))
	case '0':
		return "\x00", 2
	case '\'':
		return "'", 2
	case 'u':
		if digits, ok := strings.CutPrefix(source[2:], "{"); ok {
			if digits, _, ok := strings.Cut(digits, "}"); ok {
				code, err := strconv.ParseUint(strings.ReplaceAll(digits, "_", ""), 16, 32)
				if err == nil && code <= utf8.MaxRune && utf8.ValidRune(rune(code)) {
					return string(rune(code)), 4 + len(digits)
				}
			}
		}
	case 'x', 'n', 'r', 't', '\\', '"':
		character, _, rest, err := strconv.UnquoteChar(source, '"')
		if err == nil {
			return string(byte(character)), len(source) - len(rest)
		}
	}
	return "", 0
}
