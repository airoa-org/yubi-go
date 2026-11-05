"""Serialization and HostAdapter transport for the upstream task FSM.

Host-owned facts (recording I/O state and metadata segment count) return with
each callback reply. All sequencing is executed by TaskSequenceFSM.
"""
import json
from dataclasses import asdict
from types import SimpleNamespace
from wit_world.imports import recorder

from airoa_core.task.context import TaskContext
from airoa_core.task.episode_selector import EpisodeSelector
from airoa_core.task.fsm import TaskSequenceFSM
from airoa_core.task.protocols import HostAdapter, FsmActions
from airoa_core.task.states import ActionCommand, TaskSequenceState
from airoa_core.task.formatting import (
    compute_episode_elapsed, compute_subtask_elapsed,
    format_fsm_status_message, format_next_task_message,
)


def display(ctx, now):
    return {
        "status": format_fsm_status_message(ctx),
        "next_task": format_next_task_message(ctx),
        "subtask_elapsed_sec": compute_subtask_elapsed(ctx, now),
        "episode_elapsed_sec": compute_episode_elapsed(ctx, now),
    }


def restore(raw):
    raw = dict(raw)
    raw["status"] = TaskSequenceState(raw.get("status", 0))
    raw["completed_episode_ids"] = set(raw.get("completed_episode_ids", []))
    return TaskContext(**raw)


def dump(ctx):
    context = asdict(ctx)
    context["status"] = ctx.status.value
    context["completed_episode_ids"] = sorted(ctx.completed_episode_ids)
    return context


class Host:
    """Every HostAdapter method reaches the host, the log calls included.
    Answering them is the host's business, not ours."""

    def __init__(self, ctx):
        self.ctx = ctx

    def __getattr__(self, name):
        if name not in HostAdapter.__dict__ or name.startswith("_"):
            raise AttributeError(name)

        def call(*args, **kwargs):
            request = {"method": name, "args": list(args), "kwargs": kwargs,
                       "segments": self.ctx.segment_count, "episode": self.ctx.tasks}
            reply = json.loads(recorder.invoke(json.dumps(request)))
            facts = reply.get("facts", {})
            if facts.keys() - {"is_recording", "segment_count", "meta_data"}:
                raise ValueError("host reply contains fields not owned by HostAdapter")
            self.ctx.__dict__.update(facts)
            return reply["value"]

        return call


def run(args, backend):
    ctx = restore(args.get("context", {}))
    if "pending" in args:
        ctx.new_tasks = args["pending"]
    host = Host(ctx)
    actions = SimpleNamespace()
    fsm = TaskSequenceFSM(
        ctx, backend=backend, selector=EpisodeSelector(backend, auto_repeat=True),
        host=host, actions=actions, now_sec=lambda: args["now"],
        gate_level_getter=lambda: args["gate_level"],
    )
    for name in FsmActions.__dict__:
        if name.startswith("_do_"):
            setattr(actions, name, getattr(fsm, name[1:]))
    methods = {
        "view": lambda: (True, ""),
        "start": fsm.do_start_recording,
        "stop_subtask": fsm.do_stop_subtask,
        "stop_recording": fsm.do_stop_recording,
        "transit": lambda action, reason="": (fsm.transit(ActionCommand(action), reason), ""),
    }
    accepted, message = methods[args["method"]](**args.get("params", {}))
    return {"context": dump(ctx), "accepted": accepted, "message": message,
            "recording": bool(ctx.is_recording), "sampled_at": args["now"], "step": ctx.cur_subtask_index,
            "subtasks": len(ctx.tasks.get("subtasks", [])), "episode": ctx.tasks,
            "display": display(ctx, args["now"])}
