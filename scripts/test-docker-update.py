#!/usr/bin/env python3
"""Offline fixtures for registry upgrades, validation and rollback."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / 'deploy/docker/update-host'
DIGEST = 'ghcr.io/mocki-toki/exetrouter@sha256:' + 'a' * 64


class Upgrade(unittest.TestCase):
    def run_upgrade(self, fail=False, schema=7, version='0.2.0', pull_fail=False):
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root = parent / 'docker'
            root.mkdir()
            tools = parent / 'tools'
            tools.mkdir()
            (root / '.env').write_text('EXETROUTER_IMAGE=exetrouter:0.1.0\nKEEP=value\n')
            (root / 'compose.yaml').write_text('fixture')
            (root / 'compose.host-network.yaml').write_text('fixture')
            (root / 'update-host').write_text('old updater')
            launcher = parent / 'launcher'
            launcher.write_text('old launcher')
            native = parent / 'native'
            native.write_text('#!/bin/sh\nprintf \'%s\\n\' \'{"latest":"v0.2.0","available":true}\'\n')
            native.chmod(0o755)
            fixtures = {
                'id': '#!/bin/sh\necho 0\n',
                'flock': '#!/bin/sh\nexit 0\n',
                'docker': '''#!/bin/sh
printf '%s\n' "$*" >> "$FIXTURE_LOG"
case "$1" in
 pull) [ "$FIXTURE_PULL_FAIL" = 0 ] ;;
 image) case "$4" in *RepoDigests*) echo "$FIXTURE_DIGEST" ;; *) echo "$FIXTURE_SCHEMA" ;; esac ;;
 create) echo fixture-container ;;
 cp) case "$2" in
     */bin/exrd) printf '#!/bin/sh\\nprintf "exetrouter %s\\\\n"\\n' "$FIXTURE_VERSION" > "$3"; chmod 755 "$3" ;;
     *) echo 'new helper' > "$3" ;;
     esac ;;
 compose) case "$*" in *'backup verify'*) echo '{"snapshot":{"schema_version":7}}' ;; esac ;;
esac
''',
                'systemctl': '''#!/bin/sh
printf '%s\n' "$*" >> "$FIXTURE_LOG"
if [ "$FIXTURE_FAIL" = 1 ] && [ "$1" = restart ] && [ ! -f "$FIXTURE_FAILED" ]; then touch "$FIXTURE_FAILED"; exit 1; fi
''',
            }
            for name, body in fixtures.items():
                p = tools / name
                p.write_text(body)
                p.chmod(0o755)
            log = parent / 'log'
            env = dict(os.environ, PATH=str(tools) + ':' + os.environ['PATH'],
                       EXRD_DEPLOYMENT_ROOT=str(root), EXRD_NATIVE_BINARY=str(native),
                       EXRD_HOST_LAUNCHER=str(launcher), FIXTURE_LOG=str(log),
                       FIXTURE_FAIL=str(int(fail)), FIXTURE_FAILED=str(parent / 'failed'),
                       FIXTURE_SCHEMA=str(schema), FIXTURE_VERSION=version,
                       FIXTURE_DIGEST=DIGEST, FIXTURE_PULL_FAIL=str(int(pull_fail)))
            result = subprocess.run(['sh', str(SCRIPT)], env=env, capture_output=True)
            return (result, (root / '.env').read_text(), native.read_text(),
                    log.read_text(), list(parent.glob('update.*')))

    def test_pull_pins_digest_and_checks_backup_before_restart(self):
        r, config, native, events, leftovers = self.run_upgrade()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('EXETROUTER_IMAGE=' + DIGEST, config)
        self.assertIn('KEEP=value', config)
        self.assertNotIn('buildx build', events)
        self.assertIn('compose.host-network.yaml', events)
        self.assertLess(events.index('backup verify'), events.index('restart exetrouter.service'))
        self.assertFalse(leftovers)

    def test_failed_health_restores_old_image_and_gateway(self):
        r, config, native, events, leftovers = self.run_upgrade(fail=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('exetrouter:0.1.0', config)
        self.assertIn('available', native)
        self.assertEqual(events.count('restart exetrouter.service'), 2)
        self.assertFalse(leftovers)

    def test_schema_change_never_stops_working_service(self):
        r, config, native, events, leftovers = self.run_upgrade(schema=8)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn(b'database schema', r.stderr)
        self.assertNotIn('restart', events)
        self.assertIn('exetrouter:0.1.0', config)
        self.assertFalse(leftovers)

    def test_wrong_version_never_mutates_deployment(self):
        r, config, native, events, leftovers = self.run_upgrade(version='0.1.0')
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn('backup create', events)
        self.assertNotIn('restart', events)
        self.assertIn('exetrouter:0.1.0', config)
        self.assertFalse(leftovers)

    def test_pull_failure_keeps_working_deployment(self):
        r, config, native, events, leftovers = self.run_upgrade(pull_fail=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn('restart', events)
        self.assertIn('exetrouter:0.1.0', config)
        self.assertFalse(leftovers)


if __name__ == '__main__':
    unittest.main()
