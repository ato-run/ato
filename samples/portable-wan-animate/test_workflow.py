import json
import unittest
from pathlib import Path

from workflow import compile_prompt

WORKFLOW = json.loads((Path(__file__).parent / "workflow.json").read_text())
# The compile step only needs each node's input specification. These tests
# exercise the mode wiring, not ComfyUI's own node classes.
OBJECT_INFO = {node["type"]: {"input": {}} for node in WORKFLOW["nodes"]}


class ModeWiring(unittest.TestCase):
    def test_animation_removes_the_replace_background_and_mask(self):
        prompt = compile_prompt(WORKFLOW, OBJECT_INFO, "animation")
        self.assertNotIn("bg_images", prompt["62"]["inputs"])
        self.assertNotIn("mask", prompt["62"]["inputs"])
        self.assertEqual(prompt["57"]["inputs"]["image"], "image.jpeg")
        self.assertEqual(prompt["63"]["inputs"]["video"], "video.mp4")

    def test_replace_keeps_the_source_background_and_mask(self):
        prompt = compile_prompt(WORKFLOW, OBJECT_INFO, "replace")
        self.assertIn("bg_images", prompt["62"]["inputs"])
        self.assertIn("mask", prompt["62"]["inputs"])

    def test_an_unknown_mode_is_refused_before_running(self):
        with self.assertRaises(ValueError):
            compile_prompt(WORKFLOW, OBJECT_INFO, "other")


if __name__ == "__main__":
    unittest.main()
