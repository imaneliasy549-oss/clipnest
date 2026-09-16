# RPM: recipe here, package built by rpmbuild

`clipnest.spec` is the recipe; the `.rpm` itself is not here, because building an
RPM needs `rpmbuild`, which this machine does not have (it is a Debian-family
machine, and installing the `rpm` package was not possible when this directory
was written).

The spec packages the **portable tarball** rather than building the project. That
is deliberate: the tarball is built on the oldest supported distribution, so the
RPM carries the same binary as the `.deb` and the same glibc floor (2.39). An RPM
built from source in a Fedora container would silently require whatever glibc
that container has, which is a newer one.

**With a container (any distribution):**

```bash
podman run --rm -v "$PWD:/w" -w /w fedora:latest bash -c '
  dnf install -y git tar gzip rpm-build &&
  make dist-tarball dist-extra &&
  rpmbuild -bb dist/fedora-x86_64/clipnest.spec \
           --define "_sourcedir $PWD/dist/tarball" \
           --define "_rpmdir $PWD/dist/rpm" &&
  find dist/rpm -name "*.rpm" -exec mv {} dist/fedora-x86_64/ \;'
```

**On a Fedora machine**, with `rpmbuild` installed and the tarball in place:

```bash
rpmbuild -bb clipnest.spec --define "_sourcedir ."
```

> `@VERSION@` in the spec has to be substituted before `rpmbuild` sees it —
> `make dist-extra` does that, in the copy it writes next to the spec. A Version
> tag cannot be a macro, so `--define "version ..."` does **not** fill it in:
> rpmbuild rejects `@VERSION@` as a version string. (The release workflow used to
> do exactly that, and every RPM job failed until it was corrected.)

The release workflow does exactly this, on both x86-64 and arm64, and attaches
the RPMs to the tag.

`rpm -qp --requires clipnest-*.rpm` prints the dependencies the spec declares
plus the shared libraries `rpmbuild` found in the binary, so the file itself says
which system it installs on.
