import {
  Activity,
  Anchor,
  ArrowRightToLine,
  Box,
  Boxes,
  Check,
  Clock,
  Copy,
  Cpu,
  Database,
  Disc,
  FileCheck,
  FileText,
  Gauge,
  HardDrive,
  Key,
  LayoutGrid,
  Layers,
  Lock,
  type LucideIcon,
  Monitor,
  Network,
  Radio,
  Scale,
  Shapes,
  Shield,
  ShieldCheck,
  Sliders,
  UserCheck,
  Workflow,
  Zap,
} from "lucide-react";

/** Mockup icon size and stroke (`svg.i` in docs/mockups/v0.2/mock.css). */
export const ICON_SIZE = 15;
export const ICON_STROKE = 1.7;

// Same icons as the sidebar's resource list (Sidebar.tsx), so a kind looks the same everywhere.
const KIND_ICONS: Record<string, LucideIcon> = {
  pod: Box,
  deployment: LayoutGrid,
  statefulset: Database,
  daemonset: Boxes,
  replicaset: Copy,
  job: Check,
  cronjob: Clock,
  endpointslice: Radio,
  horizontalpodautoscaler: Gauge,
  poddisruptionbudget: Sliders,
  service: Network,
  ingress: ArrowRightToLine,
  configmap: FileText,
  secret: Lock,
  resourcequota: Scale,
  limitrange: Sliders,
  priorityclass: Activity,
  runtimeclass: Cpu,
  lease: Key,
  persistentvolume: Disc,
  persistentvolumeclaim: HardDrive,
  storageclass: Layers,
  volumeattachment: Anchor,
  node: Monitor,
  namespace: Layers,
  event: Zap,
  serviceaccount: UserCheck,
  role: Key,
  clusterrole: Shield,
  rolebinding: Workflow,
  clusterrolebinding: Workflow,
  networkpolicy: Shield,
  mutatingwebhookconfiguration: FileCheck,
  validatingwebhookconfiguration: FileCheck,
  validatingadmissionpolicy: ShieldCheck,
  validatingadmissionpolicybinding: Workflow,
  gateway: Network,
  httproute: ArrowRightToLine,
  grpcroute: ArrowRightToLine,
  gatewayclass: Network,
  application: LayoutGrid,
  appproject: Layers,
  kustomization: Workflow,
  helmrelease: Boxes,
  nodepool: Monitor,
  nodeclaim: Monitor,
  certificate: ShieldCheck,
  certificaterequest: FileCheck,
  issuer: Key,
  clusterissuer: Key,
  externalsecret: Lock,
  secretstore: Database,
  clustersecretstore: Database,
  rollout: Workflow,
  analysisrun: Activity,
};

export function KindIcon({ kind, className }: { kind: string; className?: string }) {
  const Icon = KIND_ICONS[kind.toLowerCase()] ?? Shapes;
  return (
    <Icon
      size={ICON_SIZE}
      strokeWidth={ICON_STROKE}
      aria-hidden="true"
      {...(className ? { className } : {})}
    />
  );
}
