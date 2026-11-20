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
LIB = (ROOT / 'crates/knotrel-server/src/lib.rs').read_text()
GRAPH = (ROOT / 'crates/knotrel-server/src/graph.rs').read_text()
LIFECYCLE = (ROOT / 'crates/knotrel-server/src/lifecycle.rs').read_text()


def struct_fields(name, source):
    match = re.search(r'struct ' + name + r' \{(.*?)\n\}', source, re.S)
    assert match, name
    return re.findall(r'^\s*(?:pub\(crate\) )?(\w+): ', match[1], re.M)


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

    def test_info_and_stats_fields_match(self):
        # The operational console reads these objects; every serialized field must be
        # documented (optional ones are absent in standalone mode or without lifecycle).
        info = set(struct_fields('ServerInfo', LIB))
        self.assertEqual(info, set(SPEC['components']['schemas']['ServerInfo']['properties']))
        stats = struct_fields('GraphSnapshot', SERVICE)
        self.assertIn('lifecycle', stats)  # flattened LifecycleCounts
        expected = (set(stats) - {'lifecycle'}) | set(struct_fields('LifecycleCounts', GRAPH))
        self.assertEqual(expected,
                         set(SPEC['components']['schemas']['GraphSnapshot']['properties']))

    def test_discriminators_map_wire_values(self):
        # Without an explicit mapping, OpenAPI implies schema names as wire values.
        schemas = SPEC['components']['schemas']
        for name, field in [('Operation', 'op'), ('Outcome', 'result')]:
            with self.subTest(name=name):
                disc = schemas[name]['discriminator']
                self.assertEqual(disc['propertyName'], field)
                refs = {item['$ref'] for item in schemas[name]['oneOf']}
                self.assertEqual(set(disc['mapping'].values()), refs)
                for value, ref in disc['mapping'].items():
                    target = schemas[ref.rsplit('/', 1)[1]]
                    self.assertEqual(target['properties'][field]['enum'], [value])

    def test_readiness_states_match(self):
        body = LIFECYCLE.split('pub(crate) enum ReadinessState {', 1)[1].split('}', 1)[0]
        snake = {re.sub(r'(?<!^)(?=[A-Z])', '_', name).lower()
                 for name in re.findall(r'(\w+),', body)}
        schemas = SPEC['components']['schemas']
        documented = (set(schemas['ReadinessReadyResponse']['properties']['status']['enum'])
                      | set(schemas['ReadinessNotReadyResponse']['properties']['status']['enum']))
        self.assertEqual(snake, documented)

    def test_version_headers_on_console_reads(self):
        for path in ['/v1/stats', '/v1/operations', '/v1/batch']:
            method = 'get' if path == '/v1/stats' else 'post'
            with self.subTest(path=path):
                headers = SPEC['paths'][path][method]['responses']['200']['headers']
                for name in ['ETag', 'X-Knotrel-Instance-Id', 'X-Knotrel-Generation',
                             'X-Knotrel-State-Version', 'X-Knotrel-Source-Position',
                             'X-Knotrel-Sync-Status']:
                    self.assertIn(name, headers)

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
