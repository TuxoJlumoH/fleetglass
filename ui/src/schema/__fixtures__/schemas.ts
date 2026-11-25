// Test fixture: apps/v1 Deployment schema as resolved by the backend (crates/core/src/schema.rs)
// from crates/core/tests/fixtures/openapi-v3-apps-v1.json, plus small hand-written schemas.
import type { K8sJsonSchema, SchemaLookup } from "../types";

export const deploymentSchema: K8sJsonSchema = {
 "description": "Deployment enables declarative updates for Pods and ReplicaSets.",
 "properties": {
  "apiVersion": {
   "description": "APIVersion defines the versioned schema of this representation of an object.",
   "type": "string"
  },
  "kind": {
   "description": "Kind is a string value representing the REST resource this object represents.",
   "type": "string"
  },
  "metadata": {
   "default": {},
   "description": "Standard object's metadata.",
   "properties": {
    "annotations": {
     "additionalProperties": {
      "type": "string"
     },
     "description": "Annotations is an unstructured key value map stored with a resource that may be set by external tools to store and retrieve arbitrary metadata.",
     "type": "object"
    },
    "labels": {
     "additionalProperties": {
      "type": "string"
     },
     "description": "Map of string keys and values that can be used to organize and categorize (scope and select) objects.",
     "type": "object"
    },
    "managedFields": {
     "description": "ManagedFields maps workflow-id and version to the set of fields that are managed by that workflow.",
     "items": {
      "description": "ManagedFieldsEntry is a workflow-id, a FieldSet and the group version of the resource that the fieldset applies to.",
      "properties": {
       "manager": {
        "description": "Manager is an identifier of the workflow managing these fields.",
        "type": "string"
       },
       "operation": {
        "description": "Operation is the type of operation which lead to this ManagedFieldsEntry being created.",
        "type": "string"
       }
      },
      "type": "object"
     },
     "type": "array"
    },
    "name": {
     "description": "Name must be unique within a namespace.",
     "type": "string"
    },
    "namespace": {
     "description": "Namespace defines the space within which each name must be unique.",
     "type": "string"
    }
   },
   "type": "object"
  },
  "spec": {
   "default": {},
   "description": "Specification of the desired behavior of the Deployment.",
   "properties": {
    "minReadySeconds": {
     "description": "Minimum number of seconds for which a newly created pod should be ready without any of its container crashing, for it to be considered available.",
     "format": "int32",
     "type": "integer"
    },
    "paused": {
     "description": "Indicates that the deployment is paused.",
     "type": "boolean"
    },
    "replicas": {
     "description": "Number of desired pods.",
     "format": "int32",
     "type": "integer"
    },
    "selector": {
     "description": "Label selector for pods.",
     "properties": {
      "matchLabels": {
       "additionalProperties": {
        "type": "string"
       },
       "description": "matchLabels is a map of {key,value} pairs.",
       "type": "object"
      }
     },
     "type": "object"
    },
    "strategy": {
     "default": {},
     "description": "The deployment strategy to use to replace existing pods with new ones.",
     "properties": {
      "rollingUpdate": {
       "description": "Rolling update config params.",
       "properties": {
        "maxSurge": {
         "description": "The maximum number of pods that can be scheduled above the desired number of pods.",
         "format": "int-or-string",
         "oneOf": [
          {
           "type": "integer"
          },
          {
           "type": "string"
          }
         ]
        },
        "maxUnavailable": {
         "description": "The maximum number of pods that can be unavailable during the update.",
         "format": "int-or-string",
         "oneOf": [
          {
           "type": "integer"
          },
          {
           "type": "string"
          }
         ]
        }
       },
       "type": "object"
      },
      "type": {
       "description": "Type of deployment.",
       "enum": [
        "Recreate",
        "RollingUpdate"
       ],
       "type": "string"
      }
     },
     "type": "object"
    },
    "template": {
     "default": {},
     "description": "Template describes the pods that will be created.",
     "properties": {
      "metadata": {
       "default": {},
       "description": "Standard object's metadata.",
       "properties": {
        "annotations": {
         "additionalProperties": {
          "type": "string"
         },
         "description": "Annotations is an unstructured key value map stored with a resource that may be set by external tools to store and retrieve arbitrary metadata.",
         "type": "object"
        },
        "labels": {
         "additionalProperties": {
          "type": "string"
         },
         "description": "Map of string keys and values that can be used to organize and categorize (scope and select) objects.",
         "type": "object"
        },
        "managedFields": {
         "description": "ManagedFields maps workflow-id and version to the set of fields that are managed by that workflow.",
         "items": {
          "description": "ManagedFieldsEntry is a workflow-id, a FieldSet and the group version of the resource that the fieldset applies to.",
          "properties": {
           "manager": {
            "description": "Manager is an identifier of the workflow managing these fields.",
            "type": "string"
           },
           "operation": {
            "description": "Operation is the type of operation which lead to this ManagedFieldsEntry being created.",
            "type": "string"
           }
          },
          "type": "object"
         },
         "type": "array"
        },
        "name": {
         "description": "Name must be unique within a namespace.",
         "type": "string"
        },
        "namespace": {
         "description": "Namespace defines the space within which each name must be unique.",
         "type": "string"
        }
       },
       "type": "object"
      },
      "spec": {
       "default": {},
       "description": "Specification of the desired behavior of the pod.",
       "properties": {
        "containers": {
         "description": "List of containers belonging to the pod.",
         "items": {
          "description": "A single application container that you want to run within a pod.",
          "properties": {
           "image": {
            "description": "Container image name.",
            "type": "string"
           },
           "imagePullPolicy": {
            "description": "Image pull policy.",
            "enum": [
             "Always",
             "IfNotPresent",
             "Never"
            ],
            "type": "string"
           },
           "name": {
            "default": "",
            "description": "Name of the container specified as a DNS_LABEL.",
            "type": "string"
           },
           "ports": {
            "description": "List of ports to expose from the container.",
            "items": {
             "description": "ContainerPort represents a network port in a single container.",
             "properties": {
              "containerPort": {
               "default": 0,
               "description": "Number of port to expose on the pod's IP address.",
               "format": "int32",
               "type": "integer"
              },
              "name": {
               "description": "If specified, this must be an IANA_SVC_NAME and unique within the pod.",
               "type": "string"
              },
              "protocol": {
               "default": "TCP",
               "description": "Protocol for port.",
               "enum": [
                "SCTP",
                "TCP",
                "UDP"
               ],
               "type": "string"
              }
             },
             "required": [
              "containerPort"
             ],
             "type": "object"
            },
            "type": "array"
           }
          },
          "required": [
           "name"
          ],
          "type": "object"
         },
         "type": "array"
        },
        "restartPolicy": {
         "description": "Restart policy for all containers within the pod.",
         "enum": [
          "Always",
          "Never",
          "OnFailure"
         ],
         "type": "string"
        }
       },
       "required": [
        "containers"
       ],
       "type": "object"
      }
     },
     "type": "object"
    }
   },
   "required": [
    "selector",
    "template"
   ],
   "type": "object"
  },
  "status": {
   "default": {},
   "description": "Most recently observed status of the Deployment.",
   "properties": {
    "readyReplicas": {
     "description": "Total number of non-terminating pods targeted by this Deployment with a Ready Condition.",
     "format": "int32",
     "type": "integer"
    },
    "replicas": {
     "description": "Total number of non-terminating pods targeted by this deployment (their labels match the selector).",
     "format": "int32",
     "type": "integer"
    }
   },
   "type": "object"
  }
 },
 "required": [
  "spec"
 ],
 "type": "object"
}
;

/** A CRD-like schema: int-or-string, preserve-unknown-fields, combinators. */
export const widgetSchema: K8sJsonSchema = {
  type: "object",
  properties: {
    apiVersion: { type: "string" },
    kind: { type: "string" },
    metadata: { type: "object" },
    spec: {
      type: "object",
      required: ["size"],
      properties: {
        size: { "x-kubernetes-int-or-string": true, anyOf: [{ type: "integer" }, { type: "string" }] },
        mode: { type: "string", enum: ["fast", "safe"], default: "safe", description: "How to run." },
        config: { type: "object", "x-kubernetes-preserve-unknown-fields": true },
        tags: { type: "object", additionalProperties: { type: "string" } },
        closed: { type: "object", additionalProperties: false },
        merged: {
          allOf: [
            { type: "object", properties: { a: { type: "string" } }, required: ["a"] },
            { properties: { b: { type: "integer" } } },
          ],
        },
        choice: {
          type: "object",
          properties: { x: { type: "string" }, y: { type: "string" } },
          required: ["x"],
          oneOf: [{ required: ["x"] }, { required: ["y"] }],
        },
        opaque: { oneOf: [{ $ref: "#/elsewhere" } as K8sJsonSchema, { type: "string" }] },
      },
    },
    status: { type: "object", properties: { ready: { type: "boolean" } } },
  },
};

export const configMapSchema: K8sJsonSchema = {
  type: "object",
  properties: {
    apiVersion: { type: "string" },
    kind: { type: "string" },
    metadata: { type: "object", properties: { name: { type: "string" } } },
    data: { type: "object", additionalProperties: { type: "string" } },
  },
};

export const fixtureLookup: SchemaLookup = (apiVersion, kind) => {
  if (apiVersion === "apps/v1" && kind === "Deployment") return deploymentSchema;
  if (apiVersion === "example.fleetglass.dev/v1" && kind === "Widget") return widgetSchema;
  if (apiVersion === "v1" && kind === "ConfigMap") return configMapSchema;
  return undefined;
};
