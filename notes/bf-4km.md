# bf-4km: ArgoCD Sync Verification for claude-print-ci WorkflowTemplate

**Date:** 2026-06-10

## Summary

Verified that ArgoCD successfully synced the `claude-print-ci` WorkflowTemplate from declarative-config to the iad-ci cluster.

## Findings

### WorkflowTemplate in iad-ci cluster

```
WorkflowTemplate/claude-print-ci
  Created: 2026-06-10T06:09:14Z
  Namespace: argo-workflows
```

Present and accessible via kubectl.

### ArgoCD Sync Status

- **App:** `argo-workflows-ns-iad-ci`
- **claude-print-ci resource:** `Sync: Synced | Health: None` ✓

The `claude-print-ci` WorkflowTemplate is fully synced. The overall app shows `OutOfSync / Degraded` due to pre-existing unrelated issues:
- Missing pdftract-related WorkflowTemplates and CronWorkflows
- Degraded ExternalSecrets (ghcr-registry, github-pdftract-release, pypi-token-pdftract)
- Several other unrelated WorkflowTemplates out of sync

These are pre-existing issues unrelated to claude-print-ci.

## Acceptance Criteria

- [x] `claude-print-ci` WorkflowTemplate is Synced in ArgoCD
- [x] WorkflowTemplate is present in iad-ci cluster (`kubectl get workflowtemplate claude-print-ci -n argo-workflows`)
- [ ] ArgoCD app overall is Synced/Healthy — **not met** (pre-existing unrelated issues)

The claude-print-ci specific criteria are met. The overall app health is a pre-existing concern outside the scope of this bead.
