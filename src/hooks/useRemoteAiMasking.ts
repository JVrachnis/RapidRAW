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
  mode: 'prompt' | 'points' | 'paint' | 'preset' | 'box';
  query?: string;
  points?: Array<[number, number, number]>;
  roiMaskB64?: string;
  preset?: string;
  agentic?: boolean;
  sam3Multirep?: boolean;
  carve?: boolean | null;
  box?: [number, number, number, number] | null;
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
        // Pass the FULL adjustments object (mirroring useAiMasking's invokes):
        // the warped-image cache in get_cached_full_warped_image hashes the
        // complete adjustments (incl. aiPatches), so a truncated transform-only
        // object risks a cache-key collision that returns a stale image.
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
            carve: options.carve ?? null,
            box: options.box ?? null,
            rotation: adjustments.rotation,
            flipHorizontal: adjustments.flipHorizontal,
            flipVertical: adjustments.flipVertical,
            orientationSteps: adjustments.orientationSteps,
            jsAdjustments: adjustments,
          },
        });

        // Merge back ONLY the mask geometry/result fields. The gateway echoes
        // request-memory fields (mode/query/preset/agentic/backend) which
        // already live in the sub-mask's parameters from the live UI; merging
        // them here would clobber a query the user edited mid-job.
        const {
          maskDataBase64,
          rotation,
          flipHorizontal,
          flipVertical,
          orientationSteps,
          alignment,
          labels,
          width,
          height,
          timings,
        } = newParameters || {};
        const resultFields = {
          maskDataBase64,
          rotation,
          flipHorizontal,
          flipVertical,
          orientationSteps,
          alignment,
          labels,
          width,
          height,
          timings,
        };
        const mergedParameters = { ...(subMask.parameters || {}), ...resultFields };
        updateSubMask(subMask.id, { parameters: mergedParameters });

        if (Array.isArray(newParameters?.labels) && newParameters.labels.length > 0) {
          toast.info(i18n.t('masks.remote.found', { labels: newParameters.labels.join(', ') }));
        }
      } catch (error) {
        const errorStr = String(error);
        if (errorStr.includes('comfyui_down')) {
          toast.error(i18n.t('masks.remote.comfyDown'));
        } else if (errorStr.includes('gpu_oom')) {
          toast.error(i18n.t('masks.remote.gpuOom'));
        } else {
          toast.error(`Remote AI Mask Failed: ${error}`);
        }
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
