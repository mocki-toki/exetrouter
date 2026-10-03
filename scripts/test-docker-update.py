#!/usr/bin/env python3
"""Exercise Docker upgrade success, rollback and schema refusal with offline process fixtures."""
import json, os, subprocess, tarfile, tempfile, unittest
from pathlib import Path
SCRIPT=Path(__file__).resolve().parent.parent/'deploy/docker/update-host'
class Upgrade(unittest.TestCase):
    def run_upgrade(self, fail=False, migrations=7):
        with tempfile.TemporaryDirectory() as temporary:
            parent=Path(temporary); root=parent/'docker'; root.mkdir(); tools=parent/'tools'; tools.mkdir()
            (root/'.env').write_text('EXETROUTER_IMAGE=exetrouter:0.1.0\nKEEP=value\n')
            (root/'compose.yaml').write_text('fixture')
            native=parent/'native'; native.write_text('#!/bin/sh\nprintf \'%s\\n\' \'{"latest":"v0.2.0","available":true}\'\n'); native.chmod(0o755)
            source=parent/'source'; (source/'src/migrations').mkdir(parents=True); (source/'deploy/docker').mkdir(parents=True)
            (source/'deploy/docker/Dockerfile').write_text('fixture')
            for i in range(migrations): (source/'src/migrations'/f'{i+1:03}_test.sql').write_text('-- fixture')
            archive=parent/'source.tar.gz'
            with tarfile.open(archive,'w:gz') as tar: tar.add(source,arcname='exetrouter-0.2.0')
            fixtures={
                'id': '#!/bin/sh\necho 0\n',
                'flock': '#!/bin/sh\nexit 0\n',
                'curl': '#!/bin/sh\nwhile [ "$#" -gt 0 ]; do if [ "$1" = -o ]; then cp "$FIXTURE_ARCHIVE" "$2"; exit; fi; shift; done\nexit 1\n',
                'docker': '''#!/bin/sh
printf '%s\n' "$*" >> "$FIXTURE_LOG"
case "$1" in
 create) echo fixture-container ;;
 cp) printf '#!/bin/sh\\nexit 0\\n' > "$3"; chmod 755 "$3" ;;
esac
''',
                'systemctl': '''#!/bin/sh
printf '%s\n' "$*" >> "$FIXTURE_LOG"
if [ "$FIXTURE_FAIL" = 1 ] && [ "$1" = restart ] && [ ! -f "$FIXTURE_FAILED" ]; then touch "$FIXTURE_FAILED"; exit 1; fi
''',
            }
            for name,body in fixtures.items(): p=tools/name; p.write_text(body); p.chmod(0o755)
            log=parent/'log'
            env=dict(os.environ, PATH=str(tools)+':'+os.environ['PATH'], EXRD_DEPLOYMENT_ROOT=str(root), EXRD_NATIVE_BINARY=str(native), FIXTURE_ARCHIVE=str(archive), FIXTURE_LOG=str(log), FIXTURE_FAIL=str(int(fail)), FIXTURE_FAILED=str(parent/'failed'))
            result=subprocess.run(['sh',str(SCRIPT)],env=env,capture_output=True)
            content=(root/'.env').read_text(); native_text=native.read_text(); events=log.read_text() if log.exists() else ''
            leftovers=list(parent.glob('update.*'))
            return result,content,native_text,events,leftovers
    def test_success_preserves_config_and_checks_backup_before_restart(self):
        r,config,native,events,leftovers=self.run_upgrade()
        self.assertEqual(r.returncode,0,r.stderr)
        self.assertIn('EXETROUTER_IMAGE=exetrouter:0.2.0',config); self.assertIn('KEEP=value',config)
        self.assertLess(events.index('backup verify'),events.index('restart exetrouter.service'))
        self.assertFalse(leftovers)
    def test_failed_health_restores_old_image_and_gateway(self):
        r,config,native,events,leftovers=self.run_upgrade(fail=True)
        self.assertNotEqual(r.returncode,0)
        self.assertIn('exetrouter:0.1.0',config); self.assertIn('available',native)
        self.assertEqual(events.count('restart exetrouter.service'),2)
        self.assertFalse(leftovers)
    def test_schema_change_never_stops_working_service(self):
        r,config,native,events,leftovers=self.run_upgrade(migrations=8)
        self.assertNotEqual(r.returncode,0); self.assertNotIn('restart',events)
        self.assertIn('exetrouter:0.1.0',config); self.assertFalse(leftovers)
if __name__=='__main__': unittest.main()
