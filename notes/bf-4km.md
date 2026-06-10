# bf-4km: ArgoCD Sync Verification for claude-print-ci WorkflowTemplate

**Date:** 2026-06-10 (re-verified 2026-06-10, third verification 2026-06-10, fourth verification 2026-06-10, fifth verification 2026-06-10, sixth verification 2026-06-10)

## Summary

Verified that ArgoCD successfully synced the `claude-print-ci` WorkflowTemplate from declarative-config to the iad-ci cluster.

## Findings

### WorkflowTemplate in iad-ci cluster

```
$ kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig get workflowtemplate claude-print-ci -n argo-workflows
NAME              AGE
claude-print-ci   6m29s
```

Labels confirm ArgoCD management: `argocd.argoproj.io/instance: argo-workflows-ns-iad-ci`

### ArgoCD Sync Status (2026-06-10, sixth verification)

- **App:** `argo-workflows-ns-iad-ci`
- **claude-print-ci resource:** `Sync: Synced` ✓
- **WorkflowTemplate templates:** `ci`
- **WorkflowTemplate confirmed present in cluster** (created 2026-06-10T06:09:14Z)
- **Overall app:** OutOfSync / Degraded (pre-existing, unrelated to claude-print-ci)

### ArgoCD Sync Status (2026-06-10, fifth verification)

- **App:** `argo-workflows-ns-iad-ci`
- **claude-print-ci resource:** `Sync: Synced` ✓
- **WorkflowTemplate age:** 18m (created at 2026-06-10T06:09:14Z)
- **WorkflowTemplate confirmed present in cluster**
- **Overall app:** OutOfSync / Degraded (pre-existing, unrelated to claude-print-ci)

### ArgoCD Sync Status (2026-06-10, fourth verification)

- **App:** `argo-workflows-ns-iad-ci`
- **claude-print-ci resource:** `Sync: Synced` ✓
- **WorkflowTemplate confirmed present in cluster**
- **Overall app:** OutOfSync / Degraded (pre-existing, unrelated to claude-print-ci)

### ArgoCD Sync Status (2026-06-10, third verification)

- **App:** `argo-workflows-ns-iad-ci`
- **claude-print-ci resource:** `Sync: Synced` ✓
- **WorkflowTemplate confirmed present in cluster** (kubectl get workflowtemplate claude-print-ci -n argo-workflows returns YAML with labels `argocd.argoproj.io/instance: argo-workflows-ns-iad-ci`)
- **Overall app:** OutOfSync / Degraded (pre-existing, unrelated to claude-print-ci)

The `claude-print-ci` WorkflowTemplate is fully synced. The overall app shows `OutOfSync / Degraded` due to pre-existing unrelated issues:
- Missing pdftract-related WorkflowTemplates and CronWorkflows (pdftract-ci, pdftract-crates-publish, etc.)
- Degraded ExternalSecrets (ghcr-registry, github-pdftract-release, pypi-token-pdftract — provider errors)
- SharedResourceWarning for resources shared with adb-relay-ns-iad-ci and argo-workflows-iad-ci apps
- Several other unrelated WorkflowTemplates out of sync (drawrace-build, hoop-ci, spaxel-build, etc.)

These are pre-existing issues unrelated to claude-print-ci.

## Acceptance Criteria

- [x] `claude-print-ci` WorkflowTemplate is Synced in ArgoCD
- [x] WorkflowTemplate is present in iad-ci cluster (`kubectl get workflowtemplate claude-print-ci -n argo-workflows`)
- [ ] ArgoCD app overall is Synced/Healthy — **not met** (pre-existing unrelated issues)

The claude-print-ci specific criteria are met. The overall app health is a pre-existing concern outside the scope of this bead.
