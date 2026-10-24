"""mini_jsonschema（写真フィード契約テストの代用の検査器）自体の確かめ。"""
import unittest

from tests import mini_jsonschema as mj


class PatternTest(unittest.TestCase):
    def test_pattern_is_a_partial_match_with_anchors(self):
        schema = {"type": "string", "pattern": "^[^/]"}
        self.assertEqual(mj.errors("a/b", schema), [])        # アンカーなしの末尾は部分一致
        self.assertTrue(mj.errors("/a", schema))
        self.assertEqual(mj.errors("abc", {"pattern": "b"}), [])   # アンカーなしは部分一致

    def test_dollar_does_not_match_before_a_trailing_newline(self):
        schema = {"type": "string", "pattern": "^([0-9]{2}:[0-9]{2})?$"}
        self.assertEqual(mj.errors("10:00", schema), [])
        self.assertEqual(mj.errors("", schema), [])
        self.assertTrue(mj.errors("10:00\n", schema))
        self.assertTrue(mj.errors("\n", schema))

    def test_dollar_inside_a_character_class_or_escaped_is_literal(self):
        self.assertEqual(mj.errors("a$", {"pattern": "^a[$]$"}), [])
        self.assertEqual(mj.errors("a$", {"pattern": r"^a\$$"}), [])
        self.assertTrue(mj.errors("a\n", {"pattern": "^a[$]?$"}))

    def test_escapes_that_differ_from_ecma_are_refused(self):
        for pattern in (r"^\d+$", r"\w", r"\s", r"\bfoo"):
            with self.assertRaises(ValueError, msg=pattern):
                mj.errors("1", {"pattern": pattern})


class VocabularyTest(unittest.TestCase):
    def test_unknown_keyword_is_found_even_when_no_data_reaches_it(self):
        nested = {"type": "array", "items": {"type": "object", "oneOf": []}}
        self.assertRaises(ValueError, mj.errors, [], nested)        # 空配列でも見つける
        defs = {"type": "array", "$defs": {"unused": {"anyOf": []}}}
        self.assertRaises(ValueError, mj.errors, [], defs)           # 使われない $defs の中も
        props = {"type": "object", "properties": {"x": {"format": "date"}}}
        self.assertRaises(ValueError, mj.errors, {}, props)          # 値がない property も

    def test_additional_properties_other_than_true_is_refused(self):
        schema = {"type": "object",
                  "properties": {"x": {"type": "object", "additionalProperties": False}}}
        self.assertRaises(ValueError, mj.errors, {}, schema)

    def test_broken_references_and_patterns_are_refused(self):
        self.assertRaises(ValueError, mj.errors, 1, {"$ref": "#/$defs/none"})
        self.assertRaises(ValueError, mj.errors, 1, {"$ref": "other.json"})
        self.assertRaises(ValueError, mj.errors, "x", {"pattern": "("})
        self.assertRaises(ValueError, mj.errors, 1, {"type": "float"})


class RefTest(unittest.TestCase):
    def test_ref_is_followed_and_sibling_constraints_are_also_checked(self):
        schema = {"$defs": {"n": {"type": "integer"}},
                  "type": "array",
                  "items": {"$ref": "#/$defs/n", "minimum": 5}}
        self.assertEqual(mj.errors([5, 6], schema), [])
        self.assertTrue(mj.errors([4], schema))        # 併記の minimum
        self.assertTrue(mj.errors(["7"], schema))      # 参照先の type
        self.assertTrue(mj.errors([1.5], schema))

    def test_nested_ref_chain(self):
        schema = {"$defs": {"a": {"$ref": "#/$defs/b"}, "b": {"type": "string", "minLength": 2}},
                  "$ref": "#/$defs/a"}
        self.assertEqual(mj.errors("ab", schema), [])
        self.assertTrue(mj.errors("a", schema))


if __name__ == "__main__":
    unittest.main()
