"""Validate OpenAPI and check its route/feeder DTO coverage against Rust sources.

Install scripts/requirements-openapi.txt before running this test.
This structural check complements, rather than replaces, HTTP integration tests.
"""
import re
import unittest
from pathlib import Path

import yaml
from openapi_spec_validator import validate

ROOT = Path(__file__).resolve().parents[1]
SPEC = yaml.safe_load((ROOT / 'docs/openapi.yaml').read_text())
API = (ROOT / 'crates/knotrel-server/src/api.rs').read_text()
SERVICE = (ROOT / 'crates/knotrel-server/src/service.rs').read_text()


class OpenApiContract(unittest.TestCase):
    def test_valid_specification(self):
        validate(SPEC)

    def test_all_registered_route_names_are_documented(self):
        route_impl = API.split('impl Route {', 1)[1].split('\n}', 1)[0]
        routes = set(re.findall(r'=> "(/[^\"]+)"', route_impl))
        self.assertTrue(routes)
        self.assertEqual(routes, set(SPEC['paths']))

    def test_feeder_dto_fields_match(self):
        names = ['SessionRequest', 'SessionResponse', 'HeartbeatRequest',
                 'HeartbeatResponse', 'FeederEventsRequest', 'FeederEventsResponse',
                 'FeederDuplicateResponse', 'PartialBatchFailure', 'GapRequest',
                 'GapResponse', 'BarrierRequest', 'BarrierResponse']
        for name in names:
            with self.subTest(name=name):
                match = re.search(r'struct ' + name + r' \{(.*?)\n\}',
                                  API + '\n' + SERVICE, re.S)
                self.assertIsNotNone(match)
                fields = set(re.findall(r'pub\(crate\) (\w+):', match[1]))
                schema = SPEC['components']['schemas'][name]
                self.assertEqual(fields, set(schema['properties']))
                optional = set(re.findall(r'pub\(crate\) (\w+): Option<', match[1]))
                # Optional response fields are serialized as null; optional request
                # fields may be omitted. All other wire fields are required.
                self.assertEqual(fields - (optional if name.endswith('Request') else set()),
                                 set(schema['required']))

    def test_feeder_headers_and_timeout_contract(self):
        for path, item in SPEC['paths'].items():
            if not path.startswith('/v1/feeder/'):
                continue
            with self.subTest(path=path):
                post = item['post']
                self.assertTrue(any(p['name'] == 'X-Knotrel-Instance-Id'
                                    and p['required'] for p in post['parameters']))
                self.assertIn('503', post['responses'])
                self.assertNotIn('504', post['responses'])
                self.assertIn('request_timeout', post['responses']['503']['description'])


if __name__ == '__main__':
    unittest.main()
