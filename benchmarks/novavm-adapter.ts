/**
 * NovaVM adapter — wraps the NovaVM REST API (port 9800) into the
 * ComputeSDK-compatible interface used by the benchmark framework.
 *
 * NovaVM boots real KVM micro-VMs with full Linux kernels, providing
 * hardware-level isolation — unlike containers used by most other providers.
 *
 * REST API endpoints:
 *   POST   /api/v1/sandboxes            → create + boot VM
 *   POST   /api/v1/sandboxes/:id/exec   → execute command via serial console
 *   DELETE /api/v1/sandboxes/:id         → destroy VM
 */

interface NovaVMOptions {
  /** Base URL of the NovaVM daemon REST API (default: http://localhost:9800) */
  baseUrl?: string;
  /** Default image for sandboxes (default: alpine:latest) */
  defaultImage?: string;
}

interface NovaSandbox {
  runCommand(cmd: string): Promise<{ exitCode: number; stdout: string; stderr: string }>;
  destroy(): Promise<void>;
}

interface NovaCompute {
  sandbox: {
    create(options?: Record<string, any>): Promise<NovaSandbox>;
  };
}

let counter = 0;

export function novavm(opts: NovaVMOptions = {}): NovaCompute {
  const baseUrl = (opts.baseUrl || process.env.NOVAVM_URL || 'http://localhost:9800').replace(/\/$/, '');
  const defaultImage = opts.defaultImage || process.env.NOVAVM_IMAGE || 'alpine:latest';

  return {
    sandbox: {
      async create(options?: Record<string, any>): Promise<NovaSandbox> {
        const sandboxId = `bench-${Date.now()}-${++counter}`;
        const image = options?.image || defaultImage;
        const vcpus = options?.vcpus || 1;
        const memory = options?.memory || 128;

        // Create sandbox via REST API
        const createRes = await fetch(`${baseUrl}/api/v1/sandboxes`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            sandbox_id: sandboxId,
            image,
            vcpus,
            memory,
          }),
        });

        if (!createRes.ok) {
          const errBody = await createRes.text();
          throw new Error(`NovaVM create failed (${createRes.status}): ${errBody}`);
        }

        const createData = await createRes.json() as { sandbox_id: string };
        const id = createData.sandbox_id || sandboxId;

        return {
          async runCommand(cmd: string): Promise<{ exitCode: number; stdout: string; stderr: string }> {
            const execRes = await fetch(`${baseUrl}/api/v1/sandboxes/${id}/exec`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ command: cmd }),
            });

            if (!execRes.ok) {
              const errBody = await execRes.text();
              throw new Error(`NovaVM exec failed (${execRes.status}): ${errBody}`);
            }

            const result = await execRes.json() as { stdout: string; stderr: string; exit_code: number };
            return {
              exitCode: result.exit_code,
              stdout: result.stdout,
              stderr: result.stderr,
            };
          },

          async destroy(): Promise<void> {
            try {
              await fetch(`${baseUrl}/api/v1/sandboxes/${id}`, {
                method: 'DELETE',
              });
            } catch {
              // Ignore destroy errors during cleanup
            }
          },
        };
      },
    },
  };
}
