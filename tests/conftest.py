import io
import os
import shutil
from pathlib import Path

import pytest


@pytest.fixture(scope="session")
def uv_binary() -> Path:
    """Path to the uv binary under test."""
    if env_bin := os.environ.get("UV_BIN"):
        return Path(env_bin)

    project_root = Path(__file__).parent.parent
    for candidate in [
        project_root / "result" / "bin" / "uv",
        project_root / "uv" / "target" / "debug" / "uv",
    ]:
        if candidate.exists():
            return candidate

    uv_path = shutil.which("uv")
    if uv_path:
        return Path(uv_path)

    pytest.skip("No uv binary found. Set UV_BIN or run 'nix build'.")


@pytest.fixture(scope="session")
def nix_available() -> bool:
    """Whether Nix is available (nix on PATH)."""
    return shutil.which("nix") is not None


@pytest.fixture
def tmp_python_dir(tmp_path: Path) -> Path:
    """Temporary directory for UV_PYTHON_INSTALL_DIR."""
    d = tmp_path / "python"
    d.mkdir()
    return d


@pytest.fixture(scope="session")
def docker_client():
    """Docker client for container-based tests."""
    try:
        import docker

        if not os.environ.get("DOCKER_HOST"):
            uid = os.getuid()
            podman_sock = f"/run/user/{uid}/podman/podman.sock"
            if os.path.exists(podman_sock):
                os.environ["DOCKER_HOST"] = f"unix://{podman_sock}"

        client = docker.from_env()
        client.ping()
        return client
    except Exception:
        pytest.skip("Docker not available")


@pytest.fixture(scope="session")
def scratch_image(docker_client):
    """A minimal empty Docker image (FROM scratch) for isolation tests."""
    image, _ = docker_client.images.build(
        fileobj=io.BytesIO(b"FROM scratch\n"),
        tag="uv-nix-scratch",
        rm=True,
    )
    return image
