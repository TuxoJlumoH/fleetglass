/**
 * Kubernetes JSON schema as resolved by the backend from the cluster's OpenAPI v3
 * document (`get_resource_schema`): self-contained, no `$ref`s.
 */
export interface K8sJsonSchema {
  description?: string;
  type?: string;
  format?: string;
  required?: string[];
  properties?: Record<string, K8sJsonSchema>;
  items?: K8sJsonSchema;
  enum?: unknown[];
  default?: unknown;
  nullable?: boolean;
  additionalProperties?: boolean | K8sJsonSchema;
  oneOf?: K8sJsonSchema[];
  anyOf?: K8sJsonSchema[];
  allOf?: K8sJsonSchema[];
  "x-kubernetes-preserve-unknown-fields"?: boolean;
  "x-kubernetes-int-or-string"?: boolean;
  "x-kubernetes-embedded-resource"?: boolean;
}

/** Schema of `apiVersion`/`kind`, or `undefined` when none is (yet) available. */
export type SchemaLookup = (apiVersion: string, kind: string) => K8sJsonSchema | undefined;

export interface DocumentInfo {
  kind?: string | undefined;
  apiVersion?: string | undefined;
}
