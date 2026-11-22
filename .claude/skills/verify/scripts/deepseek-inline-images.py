import argparse
import json
from pathlib import Path
import runpy
import tempfile


RESUME = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
FIXTURE = Path(__file__).resolve().parents[4] / "go/cmd/goap/testdata/parity/deepseek-switch/child.sh"
FLASH = "deepseek-v4-flash"
PRO = "deepseek-v4-pro"
PNG = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8"
    "/x8AAwMCAO+a4NEAAAAASUVORK5CYII="
)
TEXT = {"type": "text", "text": "hello"}
IMAGE = {"type": "image", "image": {"data": PNG, "media_type": "image/png"}}
ENCODED = {"type": "image", "data": PNG, "mimeType": "image/png"}


def verify(binary, verb, root, configured, model, advertises_images, supports_images):
    root.mkdir()
    (root / "home").mkdir()
    child = root / "child.sh"
    idle = 'case "$line" in *\'"model":"deepseek-chat"\'*) idle ;; esac\n'
    fixture = FIXTURE.read_text().replace(idle, "").replace("deepseek-chat", model)
    if configured != model:
        fixture = fixture.replace("\ntake\n", "\n" + idle.replace("deepseek-chat", configured) + "take\n", 1)
    child.write_text(fixture)
    child.chmod(0o700)
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"deepseek": {
        "type": "deepseek", "executable": str(child), "working_directory": str(root),
        "provider": "fixture-provider", "model": configured, "environment": ["PATH"],
    }}}))
    hub = RESUME["Hub"]([binary, verb, "--stdio", "--config", str(config)], root, "hub")

    def submit(parts):
        request = RESUME["envelope"]("session.message.submit.request", {
            "session_id": "s1", "delivery": "auto",
            "messages": [{"role": "user", "content": parts}],
        })
        return hub.call("submit", session_id="s1", request=request)

    try:
        capabilities = hub.call("capabilities", adapter="deepseek")["result"]
        features = capabilities["payload"]["features"]
        assert list(features) == sorted(features), list(features)
        feature = features.get("content.image")
        RESUME["opened"](hub, "s1")
        if model != configured:
            request = RESUME["envelope"]("session.model.switch.request", {"session_id": "s1", "model_id": model})
            switched = hub.call("session.model.switch", session_id="s1", request=request)
            assert switched["ok"] and switched["result"]["payload"]["model_id"] == model, switched
        assert hub.call("events", session_id="s1")["ok"]
        url = {"type": "image", "image": {"url": "https://example.invalid/pixel.png"}}
        refused = submit([url])
        assert not refused["ok"] and refused["error"]["code"] == "invalid_submission", refused
        reply = submit([TEXT, IMAGE])
        if supports_images:
            assert reply["ok"], f"{model} refused inline PNG: {reply}"
            expected = [TEXT, ENCODED]
        else:
            assert not reply["ok"] and reply["error"]["code"] == "unsupported_feature", reply
            assert submit("hello")["ok"], f"{model} refused text after rejecting an image"
            expected = [TEXT]
        if advertises_images:
            assert feature and feature["level"] == "native", feature
        else:
            assert not feature or feature["level"] == "unavailable", feature
        hub.completed("s1")
        starts = [line["envelope"] for line in hub.seen
                  if line.get("envelope", {}).get("type") == "run.started"]
        assert [event["capability_revision"] for event in starts] == [capabilities["capability_revision"]], starts
        hub.stop()
    finally:
        if hub.process.poll() is None:
            hub.process.kill()
            hub.process.wait(timeout=10)
            hub.errors.close()

    calls = [json.loads(line) for line in (root / "stdin.log").read_text().splitlines() if line.startswith("{")]
    models = [call["params"]["model"] for call in calls if call["method"] == "initialize"]
    assert models == ([configured, model] if model != configured else [model]), models
    prompts = [call["params"]["contentBlocks"] for call in calls if call["method"] == "session/prompt"]
    assert prompts == [expected], prompts
    print(f"{configured} → {model}: hub exit={hub.process.returncode}, 1 completed turn, 1 native prompt, "
          f"image advertised={advertises_images}, image forwarded={supports_images}")
    return capabilities["capability_revision"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--verb", choices=("hub", "serve"), default="serve")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="oxap-deepseek-images-") as temp:
        cases = [(FLASH, FLASH, True, True), ("deepseek-flash", "deepseek-flash", True, True),
                 ("deepseek-v4-flash-vision-exp", "deepseek-v4-flash-vision-exp", True, True),
                 (PRO, PRO, False, False), ("deepseek-chat", "deepseek-chat", False, False),
                 (PRO, FLASH, False, True), (FLASH, PRO, True, False)]
        revisions = {}
        for i, (configured, model, advertised, supported) in enumerate(cases):
            revision = verify(str(args.binary.resolve()), args.verb, Path(temp).resolve() / str(i),
                              configured, model, advertised, supported)
            assert revisions.setdefault(advertised, revision) == revision, revisions
        assert revisions[True] != revisions[False], revisions
    print(f"PASS: {len(cases)} DeepSeek model routes")
