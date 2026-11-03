import argparse
import functools
import http.server
import importlib.util
import os
import pwd
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('repository_build', HERE / 'build.py')
repository = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repository)


class InstallerTests(unittest.TestCase):
    def test_variant_options_and_print_plan_do_not_mutate_the_system(self):
        with tempfile.TemporaryDirectory() as root:
            installer = Path(root) / 'install.sh'
            installer.write_text((HERE / 'install.sh.in').read_text()
                                 .replace('@REPO_URL@', 'https://test.github.io/zalo')
                                 .replace('@RELEASE_URL@', 'https://github.com/test/zalo/releases/latest/download')
                                 .replace('@KEY_FINGERPRINT@', 'A' * 40))
            for option, package in [('--original', 'zalo-for-linux'), ('--zadark', 'zalo-for-linux-zadark')]:
                result = subprocess.run(['bash', str(installer), option, '--print-plan'], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('Package: ' + package + '\n', result.stdout)
            invalid = subprocess.run(['bash', str(installer), '--variant', 'unknown'], capture_output=True)
            self.assertEqual(invalid.returncode, 2)
            missing = subprocess.run(['bash', str(installer), '--variant'], capture_output=True)
            self.assertEqual(missing.returncode, 2)


@unittest.skipUnless(all(shutil.which(tool) for tool in ['gpg', 'gpgv', 'apt-ftparchive', 'dpkg-deb', 'apt-get', 'apt-cache']), 'APT and GPG tools required')
class SignedAPTTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            probe.close()
        except PermissionError as error:
            raise unittest.SkipTest('GPG agent/HTTP fixtures need sockets; sandbox denies socket creation') from error
        cls.temp = tempfile.TemporaryDirectory(prefix='zalo-repository-test-')
        cls.root = Path(cls.temp.name)
        cls.old_home = os.environ.get('GNUPGHOME')
        cls.gpg_home = cls.root / 'gnupg'
        cls.gpg_home.mkdir(mode=0o700)
        os.environ['GNUPGHOME'] = str(cls.gpg_home)
        subprocess.run(['gpg', '--batch', '--pinentry-mode', 'loopback', '--passphrase', '', '--quick-gen-key', 'Disposable repository fixture <test@example.invalid>', 'rsa2048', 'sign', '1d'], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        keys = subprocess.check_output(['gpg', '--batch', '--with-colons', '--list-secret-keys'], text=True)
        cls.key = next(line.split(':')[9] for line in keys.splitlines() if line.startswith('fpr:'))

    @classmethod
    def tearDownClass(cls):
        subprocess.run(['gpgconf', '--kill', 'gpg-agent'], check=False)
        if cls.old_home is None:
            os.environ.pop('GNUPGHOME', None)
        else:
            os.environ['GNUPGHOME'] = cls.old_home
        cls.temp.cleanup()

    def fixtures(self, root, release):
        source = root / 'input'
        source.mkdir()
        for name, label in [('zalo-for-linux', 'Original'), ('zalo-for-linux-zadark', '+ZaDark-1')]:
            package = root / name
            (package / 'DEBIAN').mkdir(parents=True)
            (package / 'DEBIAN' / 'control').write_text(f'Package: {name}\nVersion: 26.1.0-{release}\nArchitecture: amd64\nMaintainer: Test <test@example.invalid>\nDescription: Disposable test package\n')
            target = source / f'Zalo-26.1.0-{label}-abcdef0-x86_64-deb-{release}.deb'
            subprocess.run(['dpkg-deb', '--build', '--root-owner-group', str(package), str(target)], check=True, stdout=subprocess.DEVNULL)
        return source

    def generate(self, root, release='123'):
        root.mkdir()
        args = argparse.Namespace(input=self.fixtures(root, release), output=root / 'generated',
                                  repo_url='https://test.github.io/zalo',
                                  github_repository='test/zalo', repository_name='zalo',
                                  key=self.key, profiles=['deb'], arches=['x86_64'])
        repository.build(args)
        return args.output

    def test_signed_flat_repository_is_accepted_by_apt_and_keeps_variant_candidates(self):
        generated = self.generate(self.root / 'apt')
        site = generated / 'site'
        uploads = generated / 'upload'
        subprocess.run(['gpg', '--batch', '--verify', str(uploads / 'InRelease')], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.assertFalse(list(site.rglob('*.deb')))
        self.assertEqual(len(list(uploads.glob('*.deb'))), 2)
        self.assertTrue((uploads / 'Packages.gz').is_file())
        installer = site / 'install.sh'
        plan = subprocess.check_output(['bash', str(installer), '--variant', 'zadark', '--print-plan'], text=True)
        self.assertIn('Package: zalo-for-linux-zadark', plan)
        bad = subprocess.run(['bash', str(installer), '--variant', 'invalid'], capture_output=True)
        self.assertEqual(bad.returncode, 2)
        keyring = generated / 'key.gpg'
        subprocess.run(['gpg', '--batch', '--output', str(keyring), '--export', self.key], check=True)

        class Handler(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(uploads)))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            state = generated / 'apt-state'
            (state / 'lists' / 'partial').mkdir(parents=True)
            sources = state / 'sources.list'
            sources.write_text(f'deb [signed-by={keyring}] http://127.0.0.1:{server.server_port}/ ./\n')
            settings = ['-o', f'Dir::Etc::sourcelist={sources}', '-o', 'Dir::Etc::sourceparts=-', '-o', f'Dir::State::lists={state / "lists"}', '-o', f'Dir::Cache={state / "cache"}', '-o', f'APT::Sandbox::User={pwd.getpwuid(os.getuid()).pw_name}', '-o', 'Dir::Etc::main=/dev/null', '-o', 'Dir::Etc::parts=-']
            result = subprocess.run(['apt-get', *settings, 'update'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            policy = subprocess.check_output(['apt-cache', *settings, 'policy', 'zalo-for-linux', 'zalo-for-linux-zadark'], text=True)
            self.assertEqual(policy.count('Candidate: 26.1.0-123'), 2, policy)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_installer_uses_latest_release_for_apt_payloads(self):
        generated = self.generate(self.root / 'latest-release', '124')
        installer = (generated / 'site' / 'install.sh').read_text()
        self.assertIn("RELEASE_URL='https://github.com/test/zalo/releases/latest/download'", installer)
        self.assertNotIn('@RELEASE_URL@', installer)


if __name__ == '__main__':
    unittest.main()
