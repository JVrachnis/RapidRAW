import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'react-toastify';
import i18n from 'i18next';
import { useEditorStore } from '../store/useEditorStore';
import { useEditorActions } from './useEditorActions';
import { Adjustments, MaskContainer } from '../utils/adjustments';
import { SubMask } from '../components/panel/right/Masks';
import { Invokes } from '../components/ui/AppProperties';

const getTransformAdjustments = (adj: Adjustments) => ({
  transformDistortion: adj.transformDistortion,
  transformVertical: adj.transformVertical,
  transformHorizontal: adj.transformHorizontal,
  transformRotate: adj.transformRotate,
  transformAspect: adj.transformAspect,
  transformScale: adj.transformScale,
  transformXOffset: adj.transformXOffset,
  transformYOffset: adj.transformYOffset,
  lensDistortionAmount: adj.lensDistortionAmount,
  lensVignetteAmount: adj.lensVignetteAmount,
  lensTcaAmount: adj.lensTcaAmount,
  lensDistortionParams: adj.lensDistortionParams,
  lensMaker: adj.lensMaker,
  lensModel: adj.lensModel,
  lensDistortionEnabled: adj.lensDistortionEnabled,
  lensTcaEnabled: adj.lensTcaEnabled,
  lensVignetteEnabled: adj.lensVignetteEnabled,
});

export type RemoteMaskStage = 'uploading' | 'queued' | 'running' | 'done' | 'error';

export interface RemoteMaskStatus {
  subMaskId: string;
  stage: RemoteMaskStage;
  queuePosition: number | null;
  progress: number | null;
  detail: string | null;
}

interface RemoteMaskStatusEventPayload {
  sub_mask_id: string;
  stage: RemoteMaskStage;
  queue_position: number | null;
  progress: number | null;
  detail: string | null;
}

export interface GenerateRemoteAiMaskOptions {
  mode: 'prompt' | 'points' | 'paint' | 'preset';
  query?: string;
  points?: Array<[number, number, number]>;
  roiMaskB64?: string;
  preset?: string;
  agentic?: boolean;
  sam3Multirep?: boolean;
}

/**
 * Manages the lifecycle of a remote-AI ("remote-ai") sub-mask: submitting the
 * generation request to the Rust `generate_remote_ai_mask` command, tracking
 * per-sub-mask progress via the "remote-mask-status" event, and cancelling an
 * in-flight job. Mirrors useAiMasking's invoke/merge/update pattern so the
 * result plugs into the same `masks` adjustments tree.
 */
export function useRemoteAiMasking() {
  const { setAdjustments } = useEditorActions();
  const [statusMap, setStatusMap] = useState<Record<string, RemoteMaskStatus>>({});
  const [available, setAvailable] = useState(false);
  const inFlight = useRef<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    invoke(Invokes.CheckRemoteMaskBackend)
      .then((result: any) => {
        if (!cancelled) setAvailable(!!result?.available);
      })
      .catch(() => {
        if (!cancelled) setAvailable(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const unlisten = listen('remote-mask-status', (e: any) => {
      const payload = e.payload as RemoteMaskStatusEventPayload;
      if (!payload?.sub_mask_id) return;
      setStatusMap((prev) => ({
        ...prev,
        [payload.sub_mask_id]: {
          subMaskId: payload.sub_mask_id,
          stage: payload.stage,
          queuePosition: payload.queue_position ?? null,
          progress: payload.progress ?? null,
          detail: payload.detail ?? null,
        },
      }));
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const updateSubMask = useCallback(
    (subMaskId: string, updatedData: any) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: prev.masks.map((c: MaskContainer) => ({
          ...c,
          subMasks: c.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
        })),
      }));
    },
    [setAdjustments],
  );

  const clearStatus = useCallback((subMaskId: string) => {
    setStatusMap((prev) => {
      if (!(subMaskId in prev)) return prev;
      const next = { ...prev };
      delete next[subMaskId];
      return next;
    });
  }, []);

  const generate = useCallback(
    async (subMask: SubMask, options: GenerateRemoteAiMaskOptions) => {
      const { selectedImage, adjustments } = useEditorStore.getState();
      if (!selectedImage?.path) return;
      if (inFlight.current.has(subMask.id)) return;

      inFlight.current.add(subMask.id);
      setStatusMap((prev) => ({
        ...prev,
        [subMask.id]: { subMaskId: subMask.id, stage: 'uploading', queuePosition: null, progress: null, detail: null },
      }));

      try {
        const jsAdjustments = getTransformAdjustments(adjustments);
        const newParameters: any = await invoke(Invokes.GenerateRemoteAiMask, {
          request: {
            subMaskId: subMask.id,
            path: selectedImage.path,
            mode: options.mode,
            query: options.query,
            points: options.points,
            roiMaskB64: options.roiMaskB64,
            preset: options.preset,
            agentic: options.agentic,
            sam3Multirep: options.sam3Multirep,
            rotation: adjustments.rotation,
            flipHorizontal: adjustments.flipHorizontal,
            flipVertical: adjustments.flipVertical,
            orientationSteps: adjustments.orientationSteps,
            jsAdjustments,
          },
        });

        const mergedParameters = { ...(subMask.parameters || {}), ...newParameters };
        updateSubMask(subMask.id, { parameters: mergedParameters });

        if (Array.isArray(newParameters?.labels) && newParameters.labels.length > 0) {
          toast.info(i18n.t('masks.remote.found', { labels: newParameters.labels.join(', ') }));
        }
      } catch (error) {
        toast.error(`Remote AI Mask Failed: ${error}`);
      } finally {
        inFlight.current.delete(subMask.id);
        clearStatus(subMask.id);
      }
    },
    [updateSubMask, clearStatus],
  );

  const cancel = useCallback(
    async (subMaskId: string) => {
      try {
        await invoke(Invokes.CancelRemoteAiMask, { subMaskId });
      } catch (error) {
        toast.error(`Failed to cancel: ${error}`);
      } finally {
        inFlight.current.delete(subMaskId);
        clearStatus(subMaskId);
      }
    },
    [clearStatus],
  );

  return {
    generate,
    cancel,
    status: statusMap,
    available,
  };
}
