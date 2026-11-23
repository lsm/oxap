package harness

import (
	"fmt"
	"testing"
	"testing/fstest"
)

func TestTheTreeSpellsNoPinOutsideTheCatalog(t *testing.T) {
	if findings := CheckLiterals(repositoryTree(t), loadCatalog(t)); len(findings) != 0 {
		t.Fatalf("pins spelled outside the catalog: %v", findings)
	}
}

func TestAPinSpelledInSourceFails(t *testing.T) {
	catalog := loadCatalog(t)
	revision := currentOf(t, &catalog, "pi").CapabilityRevision
	digest := currentOf(t, &catalog, "claude-code").Artifacts[0].SHA256
	t.Run("go", func(t *testing.T) {
		tree := fstest.MapFS{"go/adapter/pi/adapter.go": {Data: []byte(fmt.Sprintf("package pi\n\nconst CapabilityRevision = %q\n", revision))}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, fmt.Sprintf("go/adapter/pi/adapter.go:3 spells the catalog's capability_revision %q", revision))
	})
	t.Run("go raw string", func(t *testing.T) {
		tree := fstest.MapFS{"go/adapter/claude/corpus_test.go": {Data: []byte("package claude\n\nvar digest = `" + digest + "`\n")}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "artifact digest")
	})
	t.Run("zig", func(t *testing.T) {
		tree := fstest.MapFS{"zig/src/adapter/pi/session.zig": {Data: []byte(fmt.Sprintf("pub const capability_revision = %q;\n", revision))}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "zig/src/adapter/pi/session.zig:1")
	})
	t.Run("zig multiline string", func(t *testing.T) {
		tree := fstest.MapFS{"zig/src/adapter/pi/session.zig": {Data: []byte("const x =\n    \\\\" + revision + "\n;\n")}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "zig/src/adapter/pi/session.zig:2")
	})
	t.Run("zig build script", func(t *testing.T) {
		tree := fstest.MapFS{"zig/build.zig": {Data: []byte(fmt.Sprintf("const pin = %q;\n", revision))}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "zig/build.zig:1")
	})
	t.Run("rust", func(t *testing.T) {
		tree := fstest.MapFS{"rust/adapter/pi/src/lib.rs": {Data: []byte(fmt.Sprintf("\nconst REVISION: &str = %q;\n", revision))}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "rust/adapter/pi/src/lib.rs:2")
	})
	t.Run("rust raw", func(t *testing.T) {
		tree := fstest.MapFS{"rust/adapter/claude/tests/corpus.rs": {Data: []byte(`const DIGEST: &str = r###"` + digest + `"###;`)}}
		assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "artifact digest")
	})
	t.Run("rust escapes", func(t *testing.T) {
		for _, text := range []string{`\x70`, `\u{7_0}`, "p\\\n    "} {
			tree := fstest.MapFS{"rust/adapter/pi/src/lib.rs": {Data: []byte(`const REVISION: &str = "` + text + revision[1:] + `";`)}}
			assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "rust/adapter/pi/src/lib.rs:1")
		}
	})
}

func TestAPinInsideALargerStringPasses(t *testing.T) {
	catalog := loadCatalog(t)
	revision := currentOf(t, &catalog, "pi").CapabilityRevision
	tree := fstest.MapFS{
		"go/adapter/pi/session_test.go":  {Data: []byte(fmt.Sprintf("package pi\n\nconst frame = %q\n", `{"revision":"`+revision+`"}`))},
		"zig/src/adapter/pi/session.zig": {Data: []byte("const frame =\n    \\\\\"" + revision + "\"\n;\n")},
		"zig/src/adapter/pi/notes.txt":   {Data: []byte(`"` + revision + `"`)},
	}
	if findings := CheckLiterals(tree, catalog); len(findings) != 0 {
		t.Fatalf("want no finding, got %v", findings)
	}
}

func TestRustPinCheckReadsSourceStrings(t *testing.T) {
	catalog := loadCatalog(t)
	revision := currentOf(t, &catalog, "pi").CapabilityRevision
	tree := fstest.MapFS{
		"rust/adapter/pi/tests/source.rs": {Data: []byte(fmt.Sprintf(`
/* outer /* inner */ %q */
// %q
const QUOTE: char = '"';
fn choose<'a>(text: &'a str) -> &'a str { text }
const FRAME: &str = r#"{"revision":%q}"#;
const REVISION: &str = %q;
`, revision, revision, revision, revision))},
		"rust/target/generated.rs": {Data: []byte(fmt.Sprintf("const PIN: &str = %q;", revision))},
	}
	assertOnly(t, CheckLiterals(tree, catalog), CodePinLiteral, "rust/adapter/pi/tests/source.rs:7")
}
