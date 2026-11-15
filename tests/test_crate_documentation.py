"""Offline regression checks for crate documentation release guards."""
import importlib.util
import pathlib
import unittest


def load(name):
    """Load a command-line script without running its entry point."""
    path = pathlib.Path(__file__).resolve().parents[1] / 'scripts' / f'{name}.py'
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class DocumentationTests(unittest.TestCase):
    """Check stale versions and tri-state build status without network access."""

    def test_stale_install_and_prose(self):
        check = load('check-crate-docs').stale_references
        self.assertEqual(check('cargo install x --version 0.1.0\nIn v0.1.0', '0.4.0'), [1, 2])
        self.assertEqual(check('127.0.0.1:8080 and 0.0.0.0', '0.4.0'), [])
        self.assertEqual(check('cargo add knotrel-core\nCurrent 0.4.0\nBefore 1.0', '0.4.0'), [])

    def test_unsupported_math(self):
        check = load('check-crate-docs').unsupported_math
        self.assertEqual(check(r'$O(V + E)$ and $u$'), [1])
        self.assertEqual(check(r'\(O(V)\) and \[O(E)\]'), [1])
        self.assertEqual(check('`O(log² V)` and `u`'), [])
        self.assertEqual(check(r'Cost \$5; use $HOME or ${HOME}'), [])

    def test_docs_rs_status(self):
        check = load('check-docs-rs').classify
        self.assertEqual(check({'rustdoc_status': True}), 'ready')
        self.assertEqual(check({'rustdoc_status': False}), 'failed')
        for data in ({}, {'rustdoc_status': None}, {'rustdoc_status': 'false'}):
            self.assertEqual(check(data), 'pending/unknown')


if __name__ == '__main__':
    unittest.main()
