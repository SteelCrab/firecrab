import { useEffect, useState } from "react";
import { ApiClientError, fetchJson } from "../api/client";
import { getPipeAuth } from "../api/pipeAuth";
import type { DeployMicroServiceRequest } from "../bindings";
import { hasServiceImageAuth } from "./ServiceAppPicker";

export async function verifyServicePlacement(
  id: string,
  image: string,
  request: DeployMicroServiceRequest,
  signal?: AbortSignal,
) {
  const accounts = await getPipeAuth(signal);
  if (!hasServiceImageAuth(image, accounts))
    throw new Error("Connect the image owner in Pipe Auth before deploying.");
  await fetchJson(
    `/api/micro-services/${encodeURIComponent(id)}/deployment/check`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request),
      signal,
    },
  );
}
export function useServicePlacement({
  id,
  image,
  network,
  port,
  domain,
  replicas,
  enabled,
}: {
  id: string;
  image: string;
  network: string;
  port: string;
  domain: string | null;
  replicas: number;
  enabled: boolean;
}) {
  const [retry, setRetry] = useState(0);
  const [state, setState] = useState<{
    key: string;
    error: string | null;
    ready: boolean;
  }>({ key: "", error: null, ready: false });
  const key = JSON.stringify([
    id,
    image,
    network,
    port,
    domain,
    replicas,
    retry,
  ]);
  useEffect(() => {
    if (!enabled) return;
    const controller = new AbortController();
    setState({ key, error: null, ready: false });
    const timer = setTimeout(async () => {
      try {
        if (
          !domain &&
          port &&
          (!Number.isInteger(Number(port)) ||
            Number(port) < 1024 ||
            Number(port) > 65536 - replicas)
        )
          throw new Error(
            `Choose a first host port from 1024 to ${65536 - replicas}. All replicas need consecutive ports.`,
          );
        await verifyServicePlacement(
          id,
          image,
          {
            ...(domain ? {} : { hostPort: Number(port) || 0 }),
            microNetworkId: network || null,
          },
          controller.signal,
        );
        if (!controller.signal.aborted)
          setState({ key, error: null, ready: true });
      } catch (cause) {
        if (controller.signal.aborted) return;
        const fields =
          cause instanceof ApiClientError
            ? Object.values(cause.apiError?.fields ?? {})
            : [];
        setState({
          key,
          error: fields.length
            ? fields.join(" ")
            : String(cause instanceof Error ? cause.message : cause),
          ready: false,
        });
      }
    }, 250);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [enabled, key, id, image, network, port, domain, replicas]);
  return {
    ready: enabled && state.key === key && state.ready,
    error: enabled && state.key === key ? state.error : null,
    retry: () => setRetry((value) => value + 1),
  };
}
