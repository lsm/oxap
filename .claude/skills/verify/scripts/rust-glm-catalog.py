#!/usr/bin/env python3
"""Check the Rust catalog's GLM endpoint and environment credential."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[4]
SOURCE = r'''
use oxap_provider_catalog::{Catalog, EnvironmentValue, Source};
fn main() {
    let catalog = Catalog::embedded();
    let id = "zai-coding-plan";
    assert_eq!(catalog.base_url(id, "openai-completions", ""), Some("https://api.z.ai/api/coding/paas/v4"));
    assert_eq!(catalog.base_url(id, "anthropic-messages", ""), Some("https://api.z.ai/api/anthropic/v1"));
    assert_eq!(catalog.models_endpoint(id), "/models");
    assert_eq!(catalog.wire_for_model(id, "glm-5.3"), "openai-completions");
    let env = [EnvironmentValue::new("ZHIPU_API_KEY", std::env::var("ZHIPU_API_KEY").unwrap())];
    let credential = catalog.api_key_for_provider(&env, id).unwrap();
    assert_eq!(credential.source, Source::Environment);
    assert_eq!(credential.key, env[0].value);
    assert!(catalog.provider(id).unwrap().headers.is_empty());
    println!("PASS: GLM catalog endpoints, discovery path, wire and environment credential");
}
'''


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    examples = ROOT / 'rust/provider-catalog/examples'
    examples.mkdir(exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', suffix='.rs', prefix='verify_glm_', dir=examples, delete=False) as f:
        source = Path(f.name)
        f.write(SOURCE)
    try:
        env = {'HOME': os.environ['HOME'], 'CARGO_HOME': os.environ['CARGO_HOME'], 'PATH': os.environ['PATH'], 'NODE_ENV': 'test', 'ZHIPU_API_KEY': 'synthetic-glm-key'}
        result = subprocess.run(['cargo', 'run', '--quiet', '-p', 'oxap-provider-catalog', '--example', source.stem], cwd=ROOT / 'rust', env=env, text=True, capture_output=True, timeout=600)
        (output / 'result.txt').write_text(result.stdout + result.stderr + f'\nexit={result.returncode}\n')
        sys.stdout.write(result.stdout + result.stderr)
        if result.returncode:
            raise SystemExit(result.returncode)
    finally:
        source.unlink()
        try:
            examples.rmdir()
        except OSError:
            pass


if __name__ == '__main__':
    main()
