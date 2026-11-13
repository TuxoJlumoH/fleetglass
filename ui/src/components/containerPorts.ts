/** `containerPort` values declared in a pod manifest, in order, without duplicates. */
export function containerPortsFromYaml(yaml: string): number[] {
  const ports: number[] = [];
  for (const match of yaml.matchAll(/^\s*-?\s*containerPort:\s*(\d+)\s*$/gm)) {
    const port = Number(match[1]);
    if (port > 0 && port <= 65535 && !ports.includes(port)) ports.push(port);
  }
  return ports;
}
