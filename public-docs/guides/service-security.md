# Serverless service security

The Security page under AI shows findings for deployed MicroServices. Access is restricted to the operator. Standalone MicroVMs and cluster-managed services are not scanned. The API and page list locally registered cluster-managed services excluded from security checks. Saved Pipe database connections do not link an image or service logs, so the connection itself is shown as not checked. If the database also runs as a supported MicroService, its service deployment is listed separately.

## CVE scanning

Install [Trivy](https://trivy.dev/docs/latest/getting-started/installation/) on the host running `firecrab-api`. Version 0.76.1 is verified. Make `trivy` available on the API service's `PATH`, or set `FIRECRAB_SECURITY_TRIVY` to an absolute executable path in the API service environment. Restart the API after changing its environment. Trivy is a separate dependency and is not installed by Firecrab.

A background worker scans the deployed image by its immutable digest. It checks new deployments, refreshes vulnerability data hourly, rescans when the database changes, and repeats completed scans every 24 hours. Failed scans retry after one hour. The operator can select Scan now to queue an earlier retry. Scans do not block service deployment. The worker downloads vulnerability data and accesses the image registry over the network. Existing Pipe Auth credentials are scoped to the image registry in a private temporary Docker configuration. The scanner honors the API service’s `SSL_CERT_FILE` and `SSL_CERT_DIR` settings for custom registry certificates. The API does not return credentials or scanner logs.

The page reports the affected package and version, CVE identifier, severity, source advisory, NVD link, and fixed version when available. Results are retained by service, deployment and digest. Previous release results are historical and must not be treated as current findings. CVE results cover supported packages detected by the scanner; an empty result does not prove the service is safe. The page warns when the image OS is out of support or not detected, and when advisories without CVE identifiers are omitted.

A missing scanner, unavailable registry authentication, unsupported image, invalid report, stale vulnerability data, timeout, or other scan failure is shown as a status rather than a clean result. The API marks a completed scan stale after 24 hours or when its vulnerability database is more than 48 hours old. The worker requires a pinned digest and a current database before it reports a scan as complete. It retains up to 20 scan records per service and removes them when the service is deleted.

## Activity signals

Firecrab counts 401 and 403 status codes from structured JSON application log records. Ten or more records in a five-minute window produce a signal. Retained activity uses one-minute buckets, so its timing is approximate. The count is evidence of a pattern, not proof of an attack. Check the application logs and authentication policy before taking action.

Signals retain the deployment and replica identifiers recorded when the logs were collected. Retained activity covers up to seven days and is bounded. Log collection gaps, non-JSON application logs, unsupported workloads, and the latest 200-chunk live sample limit coverage. The ingress collector also counts 401 and 403 responses for deployment-owned routes. Ingress sends the response status and route identity through a local Unix socket. The API accepts events only from the systemd ingress user. It stores only aggregated status counts and service, release, and matched replica IDs. It does not store client addresses, request paths, or raw ingress logs. The same request can appear in both ingress and application signals; their counts must not be added together. Ingress collection is best effort and may miss events. The page reports whether the collector is unavailable or has detected loss. Existing Pipe routes must be reapplied after upgrading before ingress collection becomes available.

## AI explanations

Verify a model in AI Sandbox and select it on the Security page. The operator can request an explanation for current CVEs or activity signals. AI receives CVE identifiers, severities and fixed versions, and aggregate activity counts. It does not receive raw log lines, request paths, addresses, registry credentials, package names, or image names. AI output is guidance, not evidence of an attack or proof that a fix has been applied.
