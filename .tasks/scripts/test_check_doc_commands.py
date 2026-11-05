"""The synopsis reductions `check_doc_commands.py` relies on: a wrong one either
lets a broken command through or fails the gate on correct notation."""
import pathlib
import tempfile
import unittest

from check_doc_commands import args_of, code_of


class ArgsOf(unittest.TestCase):
    def test_optional_groups_and_placeholders(self):
        self.assertEqual(
            args_of("package list [-r <r>] [--search <q>] [--blocked]"), [["package", "list"]]
        )
        self.assertEqual(args_of("why npm/<name>@<version>"), [["why", "npm/X@X"]])

    def test_an_id_placeholder_is_a_uuid(self):
        self.assertEqual(
            args_of("auth token delete <token-id>"),
            [["auth", "token", "delete", "00000000-0000-0000-0000-000000000000"]],
        )

    def test_each_alternative_is_checked(self):
        self.assertEqual(
            args_of("registry list|show"), [["registry", "list"], ["registry", "show"]]
        )
        self.assertEqual(args_of("completion <bash|zsh>"), [["completion", "bash"]])

    def test_the_shell_around_the_command_is_cut(self):
        self.assertEqual(args_of("check --strict && npm ci"), [["check", "--strict"]])
        self.assertEqual(args_of("audit log | jq ."), [["audit", "log"]])
        self.assertEqual(args_of("package readme a/b > README.md"), [["package", "readme", "a/b"]])


class CodeOf(unittest.TestCase):
    def test_prose_is_not_code_and_continuations_join(self):
        page = (
            "The batlehub server started in the same process.\n"
            "Run `batlehub-cli why a/b@1` first.\n"
            "```bash\n"
            "batlehub-cli grant set a/b \\\n"
            "  --subject user:x\n"
            "```\n"
        )
        with tempfile.TemporaryDirectory() as d:
            md = pathlib.Path(d, "p.md")
            md.write_text(page)
            self.assertEqual(
                list(code_of(md)),
                [(2, "batlehub-cli why a/b@1"), (4, "batlehub-cli grant set a/b  --subject user:x")],
            )


if __name__ == "__main__":
    unittest.main()
