# /// script
# requires-python = ">=3.11"
# dependencies = ["anthropic>=1"]
# ///
"""The AI test: gives a model short Fennec tasks with the language page as its guide, compiles and
runs each answer, and counts how many rounds it took to compile and which answers were wrong,
apart from the writes that were lost to a copy.

    uv run tools/ai-test/run.py --check-tasks        # the reference answers, no model, no cost
    uv run tools/ai-test/run.py --tasks 5            # the first five tasks
    uv run tools/ai-test/run.py                      # every task

To use answers written some other way, like by agents, with no API calls:

    uv run tools/ai-test/run.py --write-prompts DIR  # what to give whoever answers
    uv run tools/ai-test/run.py --answers DIR        # score what they wrote

`--write-prompts` writes `system.md`, the instructions and the language page, and `<task>.md` for
each task. `--answers` reads `<task>.fen` for each task, or `<task>.1.fen`, `<task>.2.fen` and so on
for each answer in turn, when errors from `fennec check` went back between them. Then the rounds
are counted as they are here: the first that compiles is the one that's run.

The key is `ANTHROPIC_API_KEY`, or else the file `~/.anthropic-token`; nothing here holds one or
prints it. Requests go to the Claude API itself, or to `FENNEC_AI_TEST_BASE_URL` if that's set,
never to an `ANTHROPIC_BASE_URL` a shell happens to have. It isn't run in CI. Results go to
`tools/ai-test/results/`, which isn't kept.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
from dataclasses import asdict, dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent

# The limits a test program runs under, as everywhere else Fennec runs one here.
SECONDS = 60
KILOBYTES = 4 * 1024 * 1024

# Dollars per million tokens: input, output, cache read, cache write.
PRICES = {
    "claude-opus-5-5": (4.00, 20.00, 0.20, 5.00),
    "claude-sonnet-5-5": (2.00, 10.00, 0.20, 2.50),
    "claude-haiku-5-5": (0.10, 0.50, 0.01, 0.125),
}

INSTRUCTIONS = """You write programs in Fennec, a new programming language. The page below is \
the source of truth for the language. Answer each task with one whole program, in a single \
```fennec code block, and nothing else in the block. The program has to compile with \
`fennec check` and print exactly what the task says when run with `fennec run`."""


@dataclass
class Program:
    """What checking and running a program did."""

    compiled: bool
    errors: str = ""
    exit_code: int | None = None
    stdout: str = ""
    stderr: str = ""
    stopped: str = ""


@dataclass
class Result:
    task: str
    topic: str
    # `passed`, `lost_write`, `wrong`, `runtime_error`, `never_compiled`, `no_code` or `refused`.
    outcome: str
    # How many answers it took to compile: 1 means the first one did.
    rounds: int
    printed: str = ""
    code: str = ""
    usage: dict = field(default_factory=dict)
    cost: float = 0.0


def run_watched(command: list[str], stdin: str = "", cwd: Path = REPO) -> tuple[int | None, str, str, str]:
    """Runs a command under the time and memory limits. Gives back its exit code, what it printed
    and, if it was stopped, why."""
    env = dict(os.environ, FENNEC_NO_CACHE="1")
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=out, stderr=err, cwd=cwd, env=env)
        process.stdin.write(stdin.encode())
        process.stdin.close()
        started = time.monotonic()
        stopped = ""
        while process.poll() is None:
            if time.monotonic() - started > SECONDS:
                stopped = f"ran more than {SECONDS} seconds"
            else:
                rss = subprocess.run(["ps", "-o", "rss=", "-p", str(process.pid)], capture_output=True, text=True)
                if rss.stdout.strip().isdigit() and int(rss.stdout) > KILOBYTES:
                    stopped = "held more than 4 GB"
            if stopped:
                process.kill()
                process.wait()
                break
            time.sleep(0.1)
        out.seek(0)
        err.seek(0)
        return process.returncode, out.read().decode(errors="replace"), err.read().decode(errors="replace"), stopped


def check_and_run(fennec: Path, source: str, stdin: str) -> Program:
    with tempfile.TemporaryDirectory() as work:
        path = Path(work) / "answer.fen"
        path.write_text(source)
        code, _, errors, stopped = run_watched([str(fennec), "check", str(path)])
        if code != 0 or stopped:
            return Program(compiled=False, errors=(stopped or errors).replace(str(path), "answer.fen"))
        code, printed, said, stopped = run_watched([str(fennec), "run", str(path)], stdin)
        return Program(compiled=True, exit_code=code, stdout=printed, stderr=said, stopped=stopped)


def judge(task: dict, program: Program) -> str:
    """What an answer that compiled did."""
    if program.stopped or program.exit_code != 0:
        return "runtime_error"
    printed = program.stdout.replace("\r\n", "\n")
    if printed == task["expected"]:
        return "passed"
    if printed in task.get("lost_write", []):
        return "lost_write"
    return "wrong"


def code_in(text: str) -> str | None:
    """The program in an answer: its ```fennec block, or its first block of any kind."""
    for pattern in (r"```fennec\n(.*?)```", r"```[a-z]*\n(.*?)```"):
        found = re.search(pattern, text, re.DOTALL)
        if found:
            return found.group(1)
    return None


def cost_of(model: str, usage: dict) -> float:
    prices = PRICES.get(model)
    if prices is None:
        return 0.0
    inputs, outputs, reads, writes = prices
    return (
        usage.get("input_tokens", 0) * inputs
        + usage.get("output_tokens", 0) * outputs
        + usage.get("cache_read_input_tokens", 0) * reads
        + usage.get("cache_creation_input_tokens", 0) * writes
    ) / 1_000_000


def ask(client, args, system: list, messages: list) -> tuple[str, dict, str]:
    """One answer from the model: its text, the tokens it took, and why it stopped."""
    response = client.beta.messages.create(
        model=args.model,
        max_tokens=16000,
        system=system,
        messages=messages,
        thinking={"type": "adaptive"},
        output_config={"effort": args.effort},
        betas=["server-side-fallback-2026-07-01"],
        fallbacks="default",
    )
    usage = {
        name: getattr(response.usage, name, 0) or 0
        for name in ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")
    }
    if response.stop_reason == "refusal":
        return "", usage, "refusal"
    text = "".join(block.text for block in response.content if block.type == "text")
    return text, usage, response.stop_reason or ""


def attempt(client, args, fennec: Path, system: list, task: dict) -> Result:
    messages = [{"role": "user", "content": task["prompt"].strip()}]
    usage: dict = {}
    code = ""
    for round_number in range(1, args.rounds + 1):
        text, used, stop = ask(client, args, system, messages)
        for name, count in used.items():
            usage[name] = usage.get(name, 0) + count
        result = Result(task["id"], task["topic"], "", round_number, usage=usage, cost=cost_of(args.model, usage))
        if stop == "refusal":
            result.outcome = "refused"
            return result
        code = code_in(text)
        if code is None:
            result.outcome = "no_code"
            return result
        result.code = code
        program = check_and_run(fennec, code, task.get("stdin", ""))
        if program.compiled:
            result.outcome = judge(task, program)
            result.printed = program.stdout + program.stderr + program.stopped
            return result
        messages += [
            {"role": "assistant", "content": text},
            {
                "role": "user",
                "content": f"`fennec check` says:\n```\n{program.errors.strip()}\n```\nAnswer with the whole program again.",
            },
        ]
    result.outcome = "never_compiled"
    result.rounds = args.rounds
    result.printed = program.errors
    return result


def check_tasks(fennec: Path, tasks: list[dict]) -> int:
    """Proves each task's reference answer compiles and prints what the task says."""
    failed = 0
    for task in tasks:
        program = check_and_run(fennec, task["reference"].lstrip(), task.get("stdin", ""))
        outcome = judge(task, program) if program.compiled else "never_compiled"
        if outcome != "passed":
            failed += 1
            shown = program.errors if not program.compiled else program.stdout + program.stderr + program.stopped
            print(f"FAILED {task['id']}: {outcome}\n{shown.strip()}\n")
        else:
            print(f"ok {task['id']}")
    print(f"{len(tasks) - failed} of {len(tasks)} reference answers pass")
    return 1 if failed else 0


def write_prompts(tasks: list[dict], folder: Path) -> int:
    """Writes what a model is given, so that answers written some other way start from the same."""
    folder.mkdir(parents=True, exist_ok=True)
    page = (REPO / "docs" / "language.md").read_text()
    (folder / "system.md").write_text(f"{INSTRUCTIONS}\n\n{page}")
    for task in tasks:
        (folder / f"{task['id']}.md").write_text(task["prompt"].strip() + "\n")
    print(f"wrote system.md and {len(tasks)} task prompts to {folder}")
    return 0


def answers_for(folder: Path, task: dict) -> list[Path]:
    """A task's answers in a folder, in the order they were written."""
    numbered = []
    for path in folder.glob(f"{task['id']}.*.fen"):
        turn = path.name.removeprefix(f"{task['id']}.").removesuffix(".fen")
        if turn.isdigit():
            numbered.append((int(turn), path))
    if numbered:
        return [path for _, path in sorted(numbered)]
    single = folder / f"{task['id']}.fen"
    return [single] if single.exists() else []


def score_answers(fennec: Path, tasks: list[dict], folder: Path) -> list[Result]:
    """Scores answers already written to a folder, with no API calls."""
    results = []
    for task in tasks:
        answers = answers_for(folder, task)
        result = Result(task["id"], task["topic"], "no_code", 0)
        for round_number, path in enumerate(answers, start=1):
            text = path.read_text()
            code = code_in(text) or text
            program = check_and_run(fennec, code, task.get("stdin", ""))
            result.rounds, result.code = round_number, code
            if program.compiled:
                result.outcome = judge(task, program)
                result.printed = program.stdout + program.stderr + program.stopped
                break
            result.outcome, result.printed = "never_compiled", program.errors
        print(f"{result.task}: {result.outcome} after {result.rounds} round(s)", flush=True)
        results.append(result)
    return results


def summary(results: list[Result], model: str) -> str:
    lines = [f"{model}, {len(results)} tasks"]
    outcomes: dict[str, int] = {}
    for result in results:
        outcomes[result.outcome] = outcomes.get(result.outcome, 0) + 1
    lines.append("outcomes: " + ", ".join(f"{name} {count}" for name, count in sorted(outcomes.items())))
    compiled = [result for result in results if result.outcome not in ("never_compiled", "no_code", "refused")]
    if compiled:
        first_time = sum(1 for result in compiled if result.rounds == 1)
        rounds = sum(result.rounds - 1 for result in compiled)
        lines.append(f"compiled first time {first_time} of {len(compiled)}; {rounds} fix rounds in all")
    topics: dict[str, list[Result]] = {}
    for result in results:
        topics.setdefault(result.topic, []).append(result)
    for topic, found in topics.items():
        passed = sum(1 for result in found if result.outcome == "passed")
        lost = sum(1 for result in found if result.outcome == "lost_write")
        lines.append(f"  {topic}: {passed} of {len(found)} passed, {lost} lost writes")
    total = sum(result.cost for result in results)
    if total > 0:
        lines.append(f"cost ${total:.2f}, ${total / max(len(results), 1):.3f} a task")
    return "\n".join(lines)


# Where the requests go unless `FENNEC_AI_TEST_BASE_URL` says otherwise.
API = "https://api.anthropic.com"


def api_key() -> str | None:
    """The key: `ANTHROPIC_API_KEY`, or the first line of `~/.anthropic-token`."""
    key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
    if key:
        return key
    try:
        return (Path.home() / ".anthropic-token").read_text().strip().splitlines()[0] or None
    except (OSError, IndexError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check-tasks", action="store_true", help="run the reference answers, with no model")
    parser.add_argument("--tasks", help="the first N tasks, or a list of ids separated by commas")
    parser.add_argument("--model", default=os.environ.get("FENNEC_AI_TEST_MODEL", "claude-opus-5-5"))
    parser.add_argument("--effort", default="medium", choices=["low", "medium", "high", "xhigh", "max"])
    parser.add_argument("--rounds", type=int, default=4, help="answers a task gets to compile")
    parser.add_argument("--fennec", type=Path, default=REPO / "target" / "debug" / "fennec")
    parser.add_argument("--write-prompts", type=Path, metavar="DIR", help="write what a model is given")
    parser.add_argument("--answers", type=Path, metavar="DIR", help="score answers in a folder, with no model")
    args = parser.parse_args()

    tasks = tomllib.loads((HERE / "tasks.toml").read_text())["task"]
    if args.tasks:
        if args.tasks.isdigit():
            tasks = tasks[: int(args.tasks)]
        else:
            wanted = args.tasks.split(",")
            tasks = [task for task in tasks if task["id"] in wanted]
    if not args.fennec.exists():
        print(f"error: there's no fennec at {args.fennec}; build it, or pass --fennec", file=sys.stderr)
        return 2
    if args.check_tasks:
        return check_tasks(args.fennec, tasks)
    if args.write_prompts:
        return write_prompts(tasks, args.write_prompts)
    if args.answers:
        results = score_answers(args.fennec, tasks, args.answers)
        return keep(results, summary(results, f"answers in {args.answers}"))

    import anthropic

    key = api_key()
    if key is None:
        print("error: set ANTHROPIC_API_KEY, or put the key in ~/.anthropic-token", file=sys.stderr)
        return 2
    client = anthropic.Anthropic(api_key=key, base_url=os.environ.get("FENNEC_AI_TEST_BASE_URL", API))
    page = (REPO / "docs" / "language.md").read_text()
    # The instructions and the language page are the same for every task, so they're cached.
    system = [
        {"type": "text", "text": INSTRUCTIONS},
        {"type": "text", "text": page, "cache_control": {"type": "ephemeral"}},
    ]
    results = []
    for task in tasks:
        result = attempt(client, args, args.fennec, system, task)
        print(f"{result.task}: {result.outcome} after {result.rounds} round(s), ${result.cost:.3f}", flush=True)
        results.append(result)
    return keep(results, summary(results, f"model {args.model}"))


def keep(results: list[Result], text: str) -> int:
    """Prints the summary and keeps it, with every answer, in `results/`."""
    print(text)
    out = HERE / "results"
    out.mkdir(exist_ok=True)
    stamp = time.strftime("%Y-%m-%d-%H%M%S")
    (out / f"{stamp}.json").write_text(json.dumps([asdict(result) for result in results], indent=2))
    (out / f"{stamp}.txt").write_text(text + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
