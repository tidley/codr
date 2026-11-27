from pathlib import Path
import unittest


class CombinedPackagingTests(unittest.TestCase):
    def test_full_linux_container_installs_zstd_before_upstream_policy_checks(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        job = workflow.split("  binaries:\n", 1)[1].split("  linux-preview:\n", 1)[0]
        self.assertIn("container: ${{ matrix.os == 'linux' && 'ubuntu:22.04' || '' }}", job)
        tools = job.split("- name: Install native build tools\n", 1)[1].split("- uses:", 1)[0]
        self.assertIn("if: runner.os == 'Linux'", tools)
        # The pinned MDK fast-ci archive fixtures invoke zstd, which is not
        # supplied by the host runner inside the Ubuntu 22.04 container.
        packages = tools.split("apt-get install -y ", 1)[1].splitlines()[0].split()
        self.assertIn("zstd", packages)
        self.assertIn("run: just fast-ci", job)
        self.assertLess(job.index("- name: Install native build tools"), job.index("run: just fast-ci"))

    def test_x64_musl_static_pie_does_not_acquire_a_runtime_interpreter(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        for name, end in (("binaries", "linux-preview"), ("linux-preview", "publish-linux-preview")):
            job = workflow.split(f"  {name}:\n", 1)[1].split(f"  {end}:\n", 1)[0]
            job_env = job.split("\n    env:\n", 1)[1].split("    steps:\n", 1)[0]
            # musl-gcc's specs add an interpreter even for Rust's -static-pie.
            # Keep PIE, but let the static CRT relocate itself without a loader.
            self.assertIn(
                '      CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS: "-C link-arg=-Wl,--no-dynamic-linker"\n',
                job_env,
            )
            self.assertIn('readelf -l ', job)
            self.assertIn('grep -q INTERP', job)
            self.assertIn('--version', job)
            self.assertIn('--help > /dev/null', job)

    def test_linux_packages_link_native_math_against_musl_not_host_glibc(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        # MDK Markdown links libm. Host cc selects glibc's static libm even
        # with Rust's musl CRT, causing unresolved __frexp/__ldexp on both CPUs.
        for name, end in (("binaries", "linux-preview"), ("linux-preview", "publish-linux-preview")):
            job = workflow.split(f"  {name}:\n", 1)[1].split(f"  {end}:\n", 1)[0]
            self.assertIn("\n    env:\n", job, f"{name} must set target-specific linkers for the whole job")
            job_env = job.split("\n    env:\n", 1)[1].split("    steps:\n", 1)[0]
            for arch in ("X86_64", "AARCH64"):
                with self.subTest(job=name, arch=arch):
                    self.assertIn(f"      CARGO_TARGET_{arch}_UNKNOWN_LINUX_MUSL_LINKER: musl-gcc\n", job_env)
            self.assertIn("musl-tools", job)

    def test_packaging_installs_target_for_the_active_pinned_toolchain(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        for name, end in (("binaries", "linux-preview"), ("linux-preview", "publish-linux-preview")):
            job = workflow.split(f"  {name}:\n", 1)[1].split(f"  {end}:\n", 1)[0]
            build = 'cargo build --locked --release --target "$TARGET"'
            self.assertIn('rustup target add "$TARGET"\n', job)
            self.assertLess(job.index('rustup target add "$TARGET"\n'), job.index(build))

    def test_compatibility_connector_is_verified_but_not_a_default_release_asset(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        full = workflow.split("  binaries:\n",1)[1].split("  linux-preview:\n",1)[0]
        self.assertIn("cargo test --locked -p agent-control -p agent-connector",full)
        self.assertIn("just fast-ci",full)
        package = full.split("- name: Package dedicated connector",1)[1].split("- uses: actions/upload-artifact",1)[0]
        self.assertIn("connector-checks/$asset",package)
        self.assertNotIn('"dist/$asset"',package)
        preview = workflow.split("- name: Verify complete Linux assets",1)[1]
        self.assertNotIn("for name in ('codr', 'codr-wn-agent')",preview)

    def test_both_worker_packaging_paths_include_embedded_dependency_licenses(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        self.assertEqual(workflow.count('python3 scripts/package_evidence.py "target/$TARGET/release/herdr-white-noise-bridge" "dist/$asset"'), 2)
        packaging = (Path(__file__).resolve().parents[1] / "scripts/package_evidence.py").read_text()
        for license in ('LICENSE', 'README.codr.md', 'LICENSE-MPL-2.0', 'NOTICE.codr'):
            self.assertIn(f'"{license}"', packaging)
        self.assertNotIn('vendor/mdk/vendor/hpke-rs-libcrux', workflow)

    def test_every_rust_build_job_installs_the_pinned_odin_compiler(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/build.yml").read_text()
        for name, end in [("checks", "checks-macos"), ("checks-macos", "binaries"),
                          ("binaries", "linux-preview"), ("linux-preview", "publish-linux-preview")]:
            job = workflow.split(f"  {name}:\n", 1)[1].split(f"  {end}:\n", 1)[0]
            self.assertIn("uses: ./.github/actions/setup-odin", job)
            self.assertLess(job.index("uses: ./.github/actions/setup-odin"), job.index("cargo "))
