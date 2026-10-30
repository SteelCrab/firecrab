# Serverless service security

The Security page under AI shows findings for deployed MicroServices. Access is restricted to the operator.

## CVE scanning

Install [Trivy](https://trivy.dev/docs/latest/getting-started/installation/) on the host running `firecrab-api`. Version 0.76.1 is verified. Make `trivy` available on the API service's `PATH`, or set `FIRECRAB_SECURITY_TRIVY` to an absolute executable path in the API service environment. Restart the API after changing its environment. Trivy is a separate dependency and is not installed by Firecrab.

A background worker scans the deployed image by its immutable digest. It checks new deployments and repeats completed scans every 24 hours. Failed scans retry after one hour. The operator can select Scan now to queue an earlier retry. Scans do not block service deployment. The worker downloads vulnerability data and accesses the image registry over the network. Existing Pipe Auth credentials are scoped to the image registry in a private temporary Docker configuration. The API does not return credentials or scanner logs.

The page reports the affected package and version, CVE identifier, severity, source advisory, NVD link, and fixed version when available. Results are retained by service, deployment and digest. Previous release results are historical and must not be treated as current findings. CVE results cover supported packages detected by the scanner; an empty result does not prove the service is safe. The page warns when the image OS is out of support or not detected, and when advisories without CVE identifiers are omitted.

A missing scanner, unavailable registry authentication, unsupported image, invalid report, stale vulnerability data, timeout, or other scan failure is shown as a status rather than a clean result. The API marks a completed scan stale after 24 hours or when its vulnerability database is more than 48 hours old. The worker requires a pinned digest and a current database before it reports a scan as complete. It retains up to 20 scan records per service and removes them when the service is deleted.

## Activity signals

Firecrab counts 401 and 403 status codes from structured JSON application log records. Ten or more records in a five-minute window produce a signal. Retained activity uses one-minute buckets, so its timing is approximate. The count is evidence of a pattern, not proof of an attack. Check the application logs and authentication policy before taking action.

Signals retain the deployment and replica identifiers recorded when the logs were collected. Retained activity covers up to seven days and is bounded. Log collection gaps, non-JSON application logs, unsupported workloads, and the latest 200-chunk live sample limit coverage. Firecrab ingress access logging is currently disabled; the Security page reports ingress logs as unavailable.

## AI explanations

Verify a model in AI Sandbox and select it on the Security page. The operator can request an explanation for current CVEs or activity signals. AI receives CVE identifiers, severities and fixed versions, and aggregate activity counts. It does not receive raw log lines, request paths, addresses, registry credentials, package names, or image names. AI output is guidance, not evidence of an attack or proof that a fix has been applied.
