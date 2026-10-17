# Security policy

## Reporting a vulnerability

Report vulnerabilities **privately**, through GitHub's private vulnerability reporting: the
"Report a vulnerability" button on this repository's Security tab opens a draft security advisory
that only the maintainers can see. Do not report them in public issues, pull requests or
discussions. If you have no GitHub account, write to the maintainers at the address in the
repository's profile and we will open the advisory for you.

Please include the version or commit, the configuration involved (with secrets removed), and the
steps or requests that reproduce the issue.

## What happens next

- **Acknowledgement within 3 business days** of the report.
- A maintainer triages the report in that window and assigns a severity with CVSS.
- **Critical issues are fixed in a release within 14 days** of the report. Other issues are fixed
  in the next scheduled release, on a timeline agreed with the reporter.
- The fix is prepared in the advisory's private fork, reviewed by two maintainers, and released
  for every supported version at the same time; the advisory is published with the release.
- **A CVE is assigned** through GitHub, as CVE numbering authority, when the advisory is published.
  The advisory and the changelog's Security section name the fixed versions.
- Reporters are credited in the advisory unless they ask not to be.

## Supported versions

The latest minor release and the one before it receive security fixes, as patch releases from
their `release/vX.Y` branches. Older versions do not. Before 1.0, only the latest release is
supported.

## Scope

In scope: the gateway binary, its official container images, the Helm chart, and the release
artefacts (archives, SBOMs, signatures and provenance).

**Insecure defaults are in scope.** A default that exposes data, opens an endpoint, disables
authentication, or fails open is a vulnerability, not a misconfiguration, and is handled under
this policy like any other.

## Safe harbour

We will not pursue good-faith research that follows this policy: avoid privacy violations, data
destruction and service disruption, test only against systems you own or are allowed to test, and
give us the time above to fix the issue before disclosing it.

## Verifying releases

Release archives and images are signed with Sigstore (keyless, bound to the release workflow),
carry SLSA build provenance, and ship a CycloneDX SBOM. The release notes give the verification
commands, and images are referenced by digest.
