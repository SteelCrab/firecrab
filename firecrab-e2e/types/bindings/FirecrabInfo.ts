// Mirrors firecrab_api_types::FirecrabInfo (camelCase wire shape), which is also
// the `--json` output of `firecrab info`.

export type FirecrabInfo = {
  /** Version of the API that answered. */
  version: string;
  /** From `$PREFIX`, or install.sh's default `/usr/local`. */
  prefix: string;
  /** From `$DATADIR`, or install.sh's default `/var/lib/firecrab`. */
  datadir: string;
  /** From `$CONFDIR`, or install.sh's default `/etc/firecrab`. */
  confdir: string;
  /** From `$UNITDIR`, or install.sh's default `/etc/systemd/system`. */
  unitdir: string;
  /** The address the API listens on, as a URL. */
  apiBase: string;
};
