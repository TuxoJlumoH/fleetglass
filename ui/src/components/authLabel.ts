import type { AuthType } from "../generated/bindings";

/** Short label for the auth badge, e.g. "exec: aws", "token", "oidc". */
export function authLabel(auth: AuthType): string {
  switch (auth.type) {
    case "exec": {
      if (!auth.command) return "exec";
      // Show the program name, not a full path (`/usr/local/bin/aws` -> `aws`).
      const name = auth.command.split(/[\\/]/).pop() ?? auth.command;
      return `exec: ${name}`;
    }
    case "authProvider":
      return `auth-provider: ${auth.name}`;
    case "clientCert":
      return "client cert";
    case "token":
    case "oidc":
    case "basic":
    case "none":
      return auth.type;
  }
}
