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


def verify(binary, verb, root, model, supports_images):
    root.mkdir()
    (root / "home").mkdir()
    child = root / "child.sh"
    child.write_text(FIXTURE.read_text().replace(
        'case "$line" in *\'"model":"deepseek-chat"\'*) idle ;; esac\n', ""
    ).replace("deepseek-chat", model))
    child.chmod(0o700)
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"deepseek": {
        "type": "deepseek", "executable": str(child), "working_directory": str(root),
        "provider": "fixture-provider", "model": model, "environment": ["PATH"],
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
        feature = capabilities["payload"]["features"].get("content.image")
        RESUME["opened"](hub, "s1")
        assert hub.call("events", session_id="s1")["ok"]
        url = {"type": "image", "image": {"url": "https://example.invalid/pixel.png"}}
        assert not submit([url])["ok"], "URL-only image was accepted"
        reply = submit([TEXT, IMAGE])
        if supports_images:
            assert reply["ok"], f"{model} refused inline PNG: {reply}"
            assert feature and feature["level"] == "native", feature
            expected = [TEXT, ENCODED]
        else:
            assert not reply["ok"], f"{model} accepted an image: {reply}"
            assert not feature or feature["level"] == "unavailable", feature
            assert submit("hello")["ok"], f"{model} refused text after rejecting an image"
            expected = [TEXT]
        hub.completed("s1")
        hub.stop()
    finally:
        if hub.process.poll() is None:
            hub.process.kill()
            hub.process.wait(timeout=10)
            hub.errors.close()

    calls = [json.loads(line) for line in (root / "stdin.log").read_text().splitlines() if line.startswith("{")]
    prompts = [call["params"]["contentBlocks"] for call in calls if call["method"] == "session/prompt"]
    assert prompts == [expected], prompts
    print(f"{model}: hub exit={hub.process.returncode}, 1 completed turn, 1 native prompt, "
          f"image advertised and forwarded={supports_images}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--verb", choices=("hub", "serve"), default="serve")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="oxap-deepseek-images-") as temp:
        cases = [(FLASH, True), ("deepseek-flash", True), ("deepseek-v4-flash-vision-exp", True),
                 (PRO, False), ("deepseek-chat", False)]
        for model, supports_images in cases:
            verify(str(args.binary.resolve()), args.verb, Path(temp).resolve() / model, model, supports_images)
    print(f"PASS: {len(cases)} DeepSeek model routes")
