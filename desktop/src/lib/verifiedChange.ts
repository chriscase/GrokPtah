export interface VerifiedChangeView {
  repositoryId?: string;
  sourceRevision?: string;
  agentId?: string;
  executionHost?: string;
  allowedFiles?: string[];
  executor?: string;
  modelSelectionKey?: string;
  cliVersion?: string | null;
  cliContract?: string | null;
  limits?: {
    maxPromptBytes?: number;
    maxRounds?: number;
    maxDurationMs?: number;
    maxTotalTokens?: number;
  };
  requiredChecks?: Array<{ checkId: string; cwd: string; timeoutMs: number }>;
  approvalRequired?: boolean;
  readiness?: {
    ready?: boolean;
    reasons?: string[];
    workersDispatched?: number;
    providerInvocations?: number;
    platform?: string;
    mutationMode?: string;
  };
  phases?: {
    workerStopped?: boolean;
    changeProposed?: boolean;
    checksPassed?: boolean;
    humanApproved?: boolean;
    applied?: boolean;
  };
  safeAction?: string;
  workId?: string | null;
  workRevision?: number | null;
  candidateDigest?: string | null;
  workState?: string;
  changedPaths?: string[];
  boundedDiff?: string;
  checkResults?: Array<{ checkId: string; outcome: string }>;
}

export function verifiedChangeReady(view: VerifiedChangeView | null | undefined): boolean {
  return view?.readiness?.ready === true && view.readiness.workersDispatched === 0;
}
