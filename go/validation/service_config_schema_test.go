package validation

import (
	"encoding/json"
	"os"
	"testing"

	"github.com/santhosh-tekuri/jsonschema/v6"
)

func TestServiceConfigSchema(t *testing.T) {
	file, err := os.Open("../../schema/server.schema.json")
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	document, err := jsonschema.UnmarshalJSON(file)
	if err != nil {
		t.Fatal(err)
	}
	const url = "https://oxap.invalid/server.schema.json"
	compiler := jsonschema.NewCompiler()
	if err := compiler.AddResource(url, document); err != nil {
		t.Fatal(err)
	}
	compiled, err := compiler.Compile(url)
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name   string
		Config any
	}
	badConfigs, err := os.ReadFile("testdata/service-invalid.json")
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(badConfigs, &cases); err != nil {
		t.Fatal(err)
	}
	if len(cases) == 0 {
		t.Fatal("no invalid fixtures")
	}
	for _, test := range cases {
		t.Run(test.Name, func(t *testing.T) {
			if err := compiled.Validate(test.Config); err == nil {
				t.Fatal("schema accepted the invalid config")
			}
		})
	}
	for _, input := range []string{
		`{"agent":{"http":"localhost:0","adapters":{"work":{"type":"claude","allowed_tools":["Read"]}}},"provider":{"stdio":"raw"}}`,
		`{"agent":{"stdio":"raw","adapter":"memory","adapters":{"memory":{"journal_capacity":0}}},"provider":{"http":"127.0.0.1:65535"}}`,
		`{"agent":{"stdio":"multi_session","tool_sources":{"tools":{"kind":"remote","auth":{"env":"TOOLS_TOKEN"}}}}}`,
	} {
		var value any
		if err := json.Unmarshal([]byte(input), &value); err != nil {
			t.Fatal(err)
		}
		if err := compiled.Validate(value); err != nil {
			t.Errorf("schema refused the valid config %s: %v", input, err)
		}
	}
	t.Logf("validated 3 good configs and refused %d bad configs", len(cases))
}
