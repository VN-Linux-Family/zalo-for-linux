#!/usr/bin/env python3
"""Build signed package-manager metadata around the latest GitHub Release."""
import argparse
import gzip
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
from urllib.parse import urlsplit

PROFILES = {'deb': '.deb', 'fedora': '.rpm', 'opensuse': '.rpm', 'arch': '.pkg.tar.xz'}
NAMES = {'zalo-for-linux', 'zalo-for-linux-zadark'}


def run(*args, cwd=None, output=None):
    result = subprocess.run(args, cwd=cwd, check=True, stdout=subprocess.PIPE if output else None)
    if output:
        Path(output).write_bytes(result.stdout)


def read_metadata(file, profile):
    if profile == 'deb':
        return tuple(subprocess.check_output(['dpkg-deb', '-f', str(file), field], text=True).strip()
                     for field in ('Package', 'Architecture'))
    if profile in ('fedora', 'opensuse'):
        return tuple(subprocess.check_output(
            ['rpm', '-qp', '--qf', '%{NAME}\n%{ARCH}\n', str(file)], text=True).splitlines())
    with tarfile.open(file, 'r:xz') as archive:
        entry = next(m for m in archive.getmembers() if m.name.removeprefix('./') == '.PKGINFO')
        data = archive.extractfile(entry).read().decode()
    values = dict(line.split(' = ', 1) for line in data.splitlines() if ' = ' in line)
    return values['pkgname'], values['arch']


def sign(file, key, clear=False):
    suffix = 'InRelease' if clear else file.name + '.asc'
    destination = file.with_name(suffix)
    run('gpg', '--batch', '--yes', '--local-user', key, '--armor',
        '--output', str(destination), '--clearsign' if clear else '--detach-sign', str(file))
    return destination


def materialize_link(link):
    if link.is_symlink():
        data = link.read_bytes()
        link.unlink()
        link.write_bytes(data)


def build(args):
    parts = urlsplit(args.repo_url)
    if (parts.scheme != 'https' or not re.fullmatch(r'[A-Za-z0-9.-]+(?::[0-9]+)?', parts.netloc)
            or parts.query or parts.fragment or not parts.path.rstrip('/').endswith('/' + args.repository_name)):
        raise ValueError('repo-url must be the HTTPS GitHub Pages URL ending in the repository name')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.github_repository):
        raise ValueError('Invalid GitHub owner/repository')
    key = args.key.upper()
    if not re.fullmatch(r'[0-9A-F]{40}|[0-9A-F]{64}', key):
        raise ValueError('Use the complete GPG key fingerprint')
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        raise ValueError('Output directory must be empty')
    site = output / 'site'
    uploads = output / 'upload'
    site.mkdir(parents=True, exist_ok=True)
    uploads.mkdir()
    keyfile = site / 'key.asc'
    run('gpg', '--batch', '--armor', '--export-options', 'export-minimal', '--export', key, output=keyfile)
    if not keyfile.stat().st_size:
        raise ValueError('Signing public key not found')
    run('gpg', '--batch', '--list-secret-keys', key)

    release_base = f'https://github.com/{args.github_repository}/releases/latest/download'
    sources = args.input.resolve()
    for profile in args.profiles:
        for arch in args.arches:
            files = sorted(sources.glob(f'Zalo-*-{arch}-{profile}-*{PROFILES[profile]}'))
            if len(files) != 2:
                raise ValueError(f'{profile}/{arch}: expected Original and ZaDark, found {len(files)}')
            names = set()
            expected_arch = {'x86_64': 'amd64', 'aarch64': 'arm64'}[arch] if profile == 'deb' else arch
            for file in files:
                name, package_arch = read_metadata(file, profile)
                if name not in NAMES or name in names or package_arch != expected_arch:
                    raise ValueError(f'Invalid or duplicate package metadata: {file.name}: {name}/{package_arch}')
                names.add(name)
            if names != NAMES:
                raise ValueError(f'{profile}/{arch}: both variants are required')

            work = output / 'work' / profile / arch
            work.mkdir(parents=True, exist_ok=True)
            copies = []
            for file in files:
                destination = work / file.name
                shutil.copy2(file, destination)
                copies.append(destination)
                if profile in ('fedora', 'opensuse'):
                    run('rpmsign', '--define', f'_gpg_name {key}',
                        '--define', f'_gpg_path {os.environ["GNUPGHOME"]}',
                        '--define', '__gpg /usr/bin/gpg', '--addsign', str(destination))
                elif profile == 'arch':
                    run('gpg', '--batch', '--yes', '--local-user', key,
                        '--output', str(destination) + '.sig', '--detach-sign', str(destination))
                shutil.copy2(destination, uploads / destination.name)
                if profile == 'arch':
                    shutil.copy2(Path(str(destination) + '.sig'), uploads / (destination.name + '.sig'))

            if profile in ('fedora', 'opensuse'):
                repository = site / 'rpm' / profile / arch
                repository.mkdir(parents=True, exist_ok=True)
                run('createrepo_c', '--checksum', 'sha256', '--baseurl', release_base,
                    '--outputdir', str(repository), str(work))
                sign(repository / 'repodata' / 'repomd.xml', key)
                shutil.copy2(keyfile, repository / 'repodata' / 'repomd.xml.key')
            elif profile == 'arch':
                database = work / 'zalo-for-linux.db.tar.gz'
                run('repo-add', '--sign', '--key', key, '--include-sigs', str(database),
                    *(str(file) for file in copies))
                for name in ('zalo-for-linux.db', 'zalo-for-linux.db.sig',
                             'zalo-for-linux.files', 'zalo-for-linux.files.sig'):
                    candidate = work / name
                    materialize_link(candidate)
                    if candidate.exists():
                        shutil.copy2(candidate, uploads / name)

    if 'deb' in args.profiles:
        pool = output / 'work' / 'deb-pool'
        pool.mkdir(parents=True, exist_ok=True)
        for arch in args.arches:
            for file in sorted((output / 'work' / 'deb' / arch).glob('*.deb')):
                shutil.copy2(file, pool / file.name)
        run('apt-ftparchive', 'packages', str(pool), output=uploads / 'Packages')
        packages = (uploads / 'Packages').read_text()
        packages = re.sub(r'^Filename: .*/([^/]+)$', r'Filename: \1', packages, flags=re.MULTILINE)
        (uploads / 'Packages').write_text(packages)
        with gzip.GzipFile(filename=str(uploads / 'Packages.gz'), mode='wb', mtime=0) as zipped:
            zipped.write((uploads / 'Packages').read_bytes())
        run('apt-ftparchive', '-o', 'APT::FTPArchive::Release::Origin=Zalo for Linux',
            '-o', 'APT::FTPArchive::Release::Label=Zalo for Linux',
            '-o', 'APT::FTPArchive::Release::Architectures=amd64 arm64',
            'release', '.', cwd=uploads, output=uploads / 'Release')
        sign(uploads / 'Release', key, clear=True)
        run('gpg', '--batch', '--yes', '--local-user', key, '--armor',
            '--output', str(uploads / 'Release.gpg'), '--detach-sign', str(uploads / 'Release'))

    template = Path(__file__).with_name('install.sh.in').read_text()
    installer = template.replace('@REPO_URL@', args.repo_url.rstrip('/'))
    installer = installer.replace('@RELEASE_URL@', release_base).replace('@KEY_FINGERPRINT@', key)
    (site / 'install.sh').write_text(installer)
    (site / 'index.txt').write_text(
        'Zalo package repository. Download /install.sh and run bash install.sh '
        '--variant original|zadark. Package payloads are in the latest GitHub Release.\n')
    shutil.rmtree(output / 'work')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--input', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--repo-url', required=True)
    parser.add_argument('--github-repository', required=True)
    parser.add_argument('--repository-name', required=True)
    parser.add_argument('--key', required=True)
    parser.add_argument('--profiles', nargs='+', choices=list(PROFILES), default=list(PROFILES))
    parser.add_argument('--arches', nargs='+', choices=['x86_64', 'aarch64'], default=['x86_64', 'aarch64'])
    build(parser.parse_args())


if __name__ == '__main__':
    main()
