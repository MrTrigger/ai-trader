# CI test checkpoints

Routine PR updates select the Python fitting-boundary tests for training changes.
Service changes run both suites because Rust defines the Python matrix contract. Whole-
boundary tests remain routine: the Rust workspace shares crate contracts, and the Python
boundary is a small suite with no narrower existing selector. Changes outside these two
known boundaries, including frontend, deployment, dependencies and workflow changes,
fail safe to both. README, changelog, license and Markdown under docs skip application
jobs on PRs.

Ready-for-review app changes, main pushes, published releases, manual dispatch and
Monday 04:23 UTC run both suites. Feature pushes do not duplicate PR validation;
superseded PR runs are cancelled. paper-image retains its existing main-path and manual
publication behavior. No release dependency, runner, image or credential changed.
Dispatch ci.yaml for full tests without image publication.

CI policy changes are checked locally with selector fixtures and actionlint. Application
builds, database services, image publication and remote workflow dispatches are not run
to validate workflow source edits. Full runtime validation occurs after delivery.

A PR opened ready for review also runs the complete checks. Root Markdown and nested
README/changelog files are documentation-only inputs on PRs.
