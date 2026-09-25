# Game discovery: OptiScaler-GUI assessment

OptiScaler-GUI was consulted as a reference for discovering local game installations and retaining platform metadata. No implementation was copied, and its graphics-injection/installer behavior is outside this engine's scope.

Useful patterns are platform-specific manifest readers, stable platform IDs, separate library roots and installations, explicit cache freshness, and a manual fallback. Discovery must distinguish installed games from ownership, subscription entitlement and AI-adapter compatibility.

For a future Rust implementation, evaluate `steamlocate` for Steam manifests and native Windows PackageManager APIs for Store/Xbox installations. Keep scanning separate from gameplay adapters. Reading a manifest does not establish that a game can be controlled.

The inspected source advertised MIT in its README, but an actual license file was not available in that checkout. Verify the permission and revision before copying code. Private machine paths and account inventory findings are not part of this public assessment.

Status: research only; no game scanner is implemented.
