# Coordinated IAM 5 release

This release separates feature OBO consent from ordinary login, refreshes the
vendored IAM client to the exact 5.0.0 release commit, and retires implicit
login-derived delegated authority. Existing users keep their ordinary sessions
and authorize delegated features when needed.

IAM client source: `f1e9c4768029aacabe337ca41be52e05023d1631`.
See `vendor/silicon-iam-client/VENDORED.md` for package provenance.

Production rollout is coordinated with IAM 5 and the receiving providers. Build
artifacts are candidates until integration checks and database backups pass.

## Follow-up 3.1.0 aggregate

This source combines schema 7 organization-scoped/private device bindings and the IAM 5 typed sign-in and feature-approval popup callbacks. Popup results remain bound to their originating window, random state, selected account, organization and environment. Changing the selected context cancels the pending approval; a delayed login cannot restore an older selection. Ordinary refresh preserves a pending approval in the same context. An uncertain completion retains its exact code and retry key; approval still does not start the original device/file action.

The CLI, SDK and service are version 3.1.0. Physical pairing protocol/native app versions are unchanged. Deploy the schema 7 service, web controls and matching CLI/SDK as a coordinated release; a rollback to globally visible device behavior is not an acceptable privacy rollback. The feature approval callback must remain the configured website's `/auth/obo/callback`.
