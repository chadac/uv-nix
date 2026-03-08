import fcntl
import os
import shutil
import subprocess
import uuid
from pathlib import Path

import pytest


_PROJECT_ROOT = Path(__file__).parent.parent
_ENTRYPOINT = Path(__file__).parent / "docker-test-lib.sh"

# Track running containers for cleanup on KeyboardInterrupt
_running_containers: set[str] = set()


def _resolve_docker_env() -> dict:
    """Resolve nix/SSL/git paths needed for Docker containers (host-side)."""
    nix_bin = os.popen("readlink -f $(which nix)").read().strip()
    git_bin = os.popen("readlink -f $(which git)").read().strip()
    git_core_dir = os.popen("git --exec-path").read().strip()
    ca_bundle = os.popen(
        "ls -d /nix/store/*-nss-cacert-*/etc/ssl/certs/ca-bundle.crt 2>/dev/null | sort | tail -1"
    ).read().strip()
    uv_bin = os.environ.get("UV_BIN", str(_PROJECT_ROOT / "uv" / "target" / "debug" / "uv"))
    return {
        "uv_bin": uv_bin,
        "nix_bin_dir": str(Path(nix_bin).parent),
        "git_bin_dir": str(Path(git_bin).parent),
        "git_core_dir": git_core_dir,
        "ca_bundle": ca_bundle,
    }


_DOCKER_ENV: dict | None = None


def _get_docker_env() -> dict:
    global _DOCKER_ENV
    if _DOCKER_ENV is None:
        _DOCKER_ENV = _resolve_docker_env()
    return _DOCKER_ENV


def _base_docker_args(env: dict, image: str) -> list[str]:
    """Common docker run arguments for all test containers."""
    return [
        "docker", "run", "--rm",
        "--network", "host",
        "-v", f"{env['uv_bin']}:/usr/local/bin/uv:ro",
        "-v", "/nix:/nix",
        "-v", f"{env['nix_bin_dir']}:/nix-bin:ro",
        "-v", f"{env['git_bin_dir']}:/git-bin:ro",
        "-v", f"{env['git_core_dir']}:/git-core:ro",
        "-e", "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin",
        "-e", "NIX_REMOTE=daemon",
        "-e", f"NIX_SSL_CERT_FILE={env['ca_bundle']}",
        "-e", f"SSL_CERT_FILE={env['ca_bundle']}",
        "-e", f"GIT_SSL_CAINFO={env['ca_bundle']}",
        "-e", f"GIT_EXEC_PATH=/git-core",
        image,
    ]


def run_in_container(
    script: str,
    image: str = "busybox",
    timeout: int = 300,
) -> subprocess.CompletedProcess:
    """Run a shell script in a container with nix mounts.

    Handles cleanup on KeyboardInterrupt by killing the container.
    """
    env = _get_docker_env()
    name = f"uv-nix-test-{uuid.uuid4().hex[:8]}"

    cmd = _base_docker_args(env, image)
    # Insert --name before the image arg
    image_idx = cmd.index(image)
    cmd[image_idx:image_idx] = ["--name", name]
    cmd.extend(["/bin/sh", "-c", script])

    _running_containers.add(name)
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (KeyboardInterrupt, subprocess.TimeoutExpired):
        subprocess.run(["docker", "kill", name], capture_output=True)
        raise
    finally:
        _running_containers.discard(name)
    return result


def run_lib_test(
    package: str,
    check: str,
    image: str = "busybox",
    no_binary: bool = False,
    timeout: int = 300,
) -> subprocess.CompletedProcess:
    """Run the library test entrypoint in a container."""
    env = _get_docker_env()
    name = f"uv-nix-test-{uuid.uuid4().hex[:8]}"

    cmd = _base_docker_args(env, image)
    image_idx = cmd.index(image)
    cmd[image_idx:image_idx] = [
        "--name", name,
        "-v", f"{_ENTRYPOINT}:/entrypoint.sh:ro",
    ]
    cmd.extend(["/bin/sh", "/entrypoint.sh", package, check])
    if no_binary:
        cmd.append("--no-binary")

    _running_containers.add(name)
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (KeyboardInterrupt, subprocess.TimeoutExpired):
        subprocess.run(["docker", "kill", name], capture_output=True)
        raise
    finally:
        _running_containers.discard(name)
    return result


@pytest.fixture(scope="session")
def warmed_image() -> str:
    """Docker image with Python 3.12 pre-installed and patched.

    Built once and persisted across sessions. Uses a file lock so
    only one xdist worker (or pytest session) builds the image;
    others wait then reuse it.
    """
    image_tag = "uv-nix-test-warmed:latest"
    lock_path = Path("/tmp/uv-nix-warmup.lock")

    with open(lock_path, "w") as lock_file:
        fcntl.flock(lock_file, fcntl.LOCK_EX)
        try:
            # Check if image already exists (another worker may have built it)
            check = subprocess.run(
                ["docker", "image", "inspect", image_tag],
                capture_output=True,
            )
            if check.returncode == 0:
                return image_tag

            env = _get_docker_env()
            name = "uv-nix-warmup"

            # Remove stale container if exists
            subprocess.run(["docker", "rm", "-f", name], capture_output=True)

            # Run uv python install in a container (no --rm, we'll commit it)
            cmd = [
                "docker", "run",
                "--name", name,
                "--network", "host",
                "-v", f"{env['uv_bin']}:/usr/local/bin/uv:ro",
                "-v", "/nix:/nix",
                "-v", f"{env['nix_bin_dir']}:/nix-bin:ro",
                "-v", f"{env['git_bin_dir']}:/git-bin:ro",
                "-v", f"{env['git_core_dir']}:/git-core:ro",
                "-e", "PATH=/usr/local/bin:/usr/bin:/bin:/nix-bin:/git-bin",
                "-e", "NIX_REMOTE=daemon",
                "-e", f"NIX_SSL_CERT_FILE={env['ca_bundle']}",
                "-e", f"SSL_CERT_FILE={env['ca_bundle']}",
                "-e", f"GIT_SSL_CAINFO={env['ca_bundle']}",
                "-e", f"GIT_EXEC_PATH=/git-core",
                "busybox",
                "/bin/sh", "-c", "uv python install 3.12",
            ]

            result = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
            assert result.returncode == 0, f"Warmup failed:\n{result.stderr}"

            # Commit the container as an image
            subprocess.run(
                ["docker", "commit", name, image_tag],
                check=True, capture_output=True,
            )
            subprocess.run(["docker", "rm", name], capture_output=True)
        finally:
            fcntl.flock(lock_file, fcntl.LOCK_UN)

    return image_tag


def pytest_keyboard_interrupt(excinfo):
    """Clean up any running containers on Ctrl+C."""
    for name in list(_running_containers):
        subprocess.run(["docker", "kill", name], capture_output=True)
