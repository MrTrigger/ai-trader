#!/usr/bin/env python3
"""Native Buildkite entrypoint. Event metadata comes from the trusted event broker."""
import fnmatch
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
ENV = os.environ


def run(command, **kwargs):
    return subprocess.run(command, check=True, cwd=ROOT, **kwargs)


def output(command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def context():
    if ENV.get('CI_REPOSITORY') != REPOSITORY:
        raise ValueError('Unexpected CI_REPOSITORY')
    if output(['git', 'rev-parse', 'HEAD']) != ENV['CI_COMMIT']:
        raise ValueError('Checkout must match the event source commit')
    if ENV['CI_EVENT'] not in {'push', 'pull_request', 'workflow_dispatch', 'release', 'schedule'}:
        raise ValueError('Unsupported CI_EVENT')


def comparison():
    base = ENV.get('CI_BASE_SHA', '')
    if not base and ENV['CI_EVENT'] == 'pull_request':
        branch = ENV['CI_BASE_BRANCH']
        run(['git', 'fetch', 'origin', '+refs/heads/' + branch + ':refs/remotes/origin/' + branch])
        return 'refs/remotes/origin/' + branch
    if base and re.fullmatch(r'[0-9a-fA-F]{40,64}', base) and set(base) != {'0'}:
        if subprocess.run(['git', 'cat-file', '-e', base + '^{commit}'], cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            run(['git', 'fetch', 'origin', base])
        return base
    return ''


def changed(patterns):
    base = comparison()
    if not base:
        return True
    try:
        if ENV['CI_EVENT'] == 'pull_request':
            base = output(['git', 'merge-base', base, ENV['CI_COMMIT']])
        paths = output(['git', 'diff', '--name-only', base, ENV['CI_COMMIT']]).splitlines()
    except subprocess.CalledProcessError:
        return True
    patterns = list(patterns) + ['.buildkite/*', 'scripts/buildkite-ci.py']
    return any(fnmatch.fnmatchcase(path, pattern) for path in paths for pattern in patterns)


def scope(keys):
    run(['python3', '.github/ci/check-toolchains.py', '--root', str(ROOT)])
    result = run(['bash', '.github/ci-scope.sh', ENV['CI_EVENT'], ENV.get('CI_PR_ACTION', ''),
                  comparison(), ENV['CI_COMMIT'], ENV.get('CI_PR_DRAFT', 'false')], capture_output=True, text=True)
    values = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
    if any(values.get(key) not in {'true', 'false'} for key in keys):
        raise ValueError('Scope script returned invalid booleans')
    return {key: values[key] == 'true' for key in keys}


def step(key, label, timeout=30, **kwargs):
    return {'key': key, 'label': label, 'command': 'python3 -B scripts/buildkite-ci.py ' + key,
            'timeout_in_minutes': timeout, **kwargs}


def upload(steps):
    if not steps:
        steps = [{'label': 'No checks selected', 'command': 'true', 'timeout_in_minutes': 1}]
    run(['buildkite-agent', 'pipeline', 'upload', '--no-interpolation'],
        input=json.dumps({'agents': {'queue': 'personal'}, 'steps': steps}), text=True)


def container(image, command, env=None):
    cache = Path(ENV.get('XDG_CACHE_HOME', str(Path.home() / '.cache'))) / 'buildkite-native' / REPOSITORY.split('/')[1]
    cache.mkdir(parents=True, exist_ok=True)
    name = 'bk-' + ENV['BUILDKITE_JOB_ID']
    args = ['docker', 'run', '--rm', '--name', name, '--label', 'ci.job=' + ENV['BUILDKITE_JOB_ID'],
            '--user', str(os.getuid()) + ':' + str(os.getgid()), '-v', str(ROOT) + ':' + str(ROOT),
            '-v', str(cache) + ':/ci-cache', '-w', str(ROOT), '-e', 'HOME=/ci-cache',
            '-e', 'CI=1']
    for key, value in (env or {}).items():
        args += ['-e', key + '=' + value]
    try:
        run([*args, image, 'bash', '-euo', 'pipefail', '-c', command])
    finally:
        subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def github(method, path, body=None):
    args = ['gh', 'api', '--method', method, path]
    kwargs = {'capture_output': True, 'text': True}
    if body is not None:
        args += ['--input', '-']
        kwargs['input'] = json.dumps(body)
    token = ENV.get('CI_GITHUB_TOKEN') or ENV.get('GH_TOKEN')
    if not token:
        raise ValueError('Scoped CI_GITHUB_TOKEN is required for GitHub metadata updates')
    kwargs['env'] = dict(ENV, GH_TOKEN=token)
    result = run(args, **kwargs)
    return json.loads(result.stdout) if result.stdout.strip() else None


def publish(repository, dockerfile='Dockerfile', tags=(), buildargs=(), provenance=True):
    if ENV['CI_EVENT'] not in {'push', 'workflow_dispatch'}:
        raise ValueError('Publication requires a trusted branch event')
    token = ENV.get('GHCR_WRITE_TOKEN')
    if not token:
        raise ValueError('Missing step-scoped GHCR_WRITE_TOKEN')
    with tempfile.TemporaryDirectory(prefix='bk-docker-auth-') as directory:
        environment = dict(ENV, DOCKER_CONFIG=directory)
        run(['docker', 'login', 'ghcr.io', '-u', ENV.get('GHCR_USERNAME', 'MrTrigger'), '--password-stdin'],
            input=token, text=True, env=environment)
        command = ['docker', 'buildx', 'build', '--file', dockerfile, '--push']
        if not provenance:
            command += ['--provenance=false']
        for tag in tags:
            command += ['--tag', repository + ':' + tag]
        for value in buildargs:
            command += ['--build-arg', value]
        run([*command, '.'], env=environment)


def interrupted(signum, frame):
    raise SystemExit(128 + signum)


REPOSITORY = 'MrTrigger/ai-trader'


def pipeline():
    event = ENV['CI_EVENT']
    workflow = ENV.get('CI_WORKFLOW', 'all')
    if workflow not in {'all', 'ci', 'image'}:
        raise ValueError('CI_WORKFLOW must be all, ci or image')
    steps = []
    if (event != 'push' or ENV['CI_BRANCH'] == 'main') and (event != 'workflow_dispatch' or workflow in {'all', 'ci'}):
        selected = scope(('app', 'full', 'rust', 'training'))
        if selected['rust']:
            steps.append(step('rust', 'Rust workspace', 30, agents={'queue': 'personal', 'capacity': 'large'}))
        if selected['training']:
            steps.append(step('training', 'Python fitting boundary', 30, agents={'queue': 'personal', 'capacity': 'large'}))
    if event in {'push', 'workflow_dispatch'} and ENV['CI_BRANCH'] == 'main' and (event != 'workflow_dispatch' or workflow in {'all', 'image'}):
        if event == 'workflow_dispatch' or changed(['service/**', 'frontend/**', 'training/**', 'config/default.toml', 'data/models/**', 'deploy/**', '.github/workflows/image.yaml']):
            steps.append(step('image', 'Publish paper trading image', 60, agents={'queue': 'personal', 'capacity': 'large'}, env={'CI_OPERATION': 'publish'}))
    upload(steps)


def rust():
    container('rust:1-bookworm', 'mkdir -p /ci-cache/cargo; cd service; cargo test --workspace',
              {'CARGO_HOME': '/ci-cache/cargo', 'RUSTUP_HOME': '/usr/local/rustup'})


def training():
    run(['docker', 'build', '-f', '.buildkite/Dockerfile.training', '-t', 'ai-trader-training:python312', '.buildkite'])
    container('ai-trader-training:python312', 'python -m venv /tmp/venv; /tmp/venv/bin/pip install -e "training[dev]"; /tmp/venv/bin/pytest -q training',
              {'PIP_CACHE_DIR': '/ci-cache/pip'})


def image():
    if ENV['CI_BRANCH'] != 'main':
        raise ValueError('Paper image publication is restricted to main')
    publish('ghcr.io/mrtrigger/aitrader-paper', 'deploy/Dockerfile', ['latest', ENV['CI_COMMIT']], provenance=False)


ACTIONS = {'rust': rust, 'training': training, 'image': image}


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    context()
    action = sys.argv[1]
    if action == 'pipeline':
        pipeline()
    elif action in ACTIONS:
        ACTIONS[action]()
    else:
        raise SystemExit('Unknown Buildkite operation: ' + action)
