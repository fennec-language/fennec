#!/bin/bash
# check-pipefail.sh [ROOT]: fails if a step of any workflow, or of an action in this repository,
# runs a pipe as a command of its own without `pipefail` in force. Under `bash -e`, such a pipe
# takes its last command's exit, so `fennec-fuzz run … | tee fuzz.txt` passed whatever the fuzzer
# found, and the nightly runs dropped every failure they found from 10-31 to 11-26.
#
# `pipefail` is in force for a step whose shell is `bash`, which GitHub runs as
# `bash --noprofile --norc -eo pipefail {0}`, set on the step, its job's `defaults` or the
# workflow's; for a custom shell naming `pipefail`; and for a script whose first command is
# `set -o pipefail` or a `set` with `-o pipefail`. With no shell named, a step runs under
# `bash -e {0}`, which has none, except on a runner that's only Windows, where PowerShell runs it.
#
# A pipe whose exit the script tests itself isn't one of these: a condition of `if`, `elif`,
# `while` or `until`, one after `!`, one in an `&&` or `||` list, and one inside `$( )`, `<( )` or
# backquotes. With `pipefail` there, `cmd | grep -q` would fail when grep stops `cmd` early. Nor
# is a `|` inside quotes, a `${{ }}`, a comment or a `case` pattern.
root=${1:-.}
cd "$root" || exit 1
# The check is Ruby's, which reads YAML and needs nothing installed on a runner.
# shellcheck disable=SC2016
exec ruby -ryaml -e '
def pipefail_set?(shell, script)
  return true if shell == "bash"
  return true if shell.is_a?(String) && shell.include?("pipefail")
  first = script.lines.map(&:strip).reject { |line| line.empty? || line.start_with?("#") }.first.to_s
  first.match?(/\Aset (-[a-z]+ )*-[a-z]*o pipefail\b/)
end

# The pipes in a script that are commands of their own, each as its line.
def lone_pipes(script)
  lines = []
  script.each_line do |line|
    text = line.chomp.gsub(/\$\{\{.*?\}\}/, "").gsub(/"(?:\\.|[^"\\])*"/, "\"\"").gsub(/\x27[^\x27]*\x27/, "\x27\x27")
    text = text.sub(/(^|\s)#.*/, "")
    # A line ending in `\` or `|` goes on to the next.
    if !lines.empty? && lines.last[:open]
      lines.last[:text] += " " + text
    else
      lines << { text: text, from: line.strip }
    end
    lines.last[:open] = text.rstrip.end_with?("\\", "|") && !text.rstrip.end_with?("||")
  end
  lines.select do |line|
    text = line[:text]
    # What the script runs inside another command, innermost first.
    nil while text.gsub!(/\$\([^()]*\)|<\([^()]*\)|>\([^()]*\)|`[^`]*`/, "")
    # A `case` pattern, `a | b)`.
    text = text.gsub(/(\bin\s+|\A\s*|;;\s*)(?:[^()|;\s]*\s*\|\s*)+[^()|;\s]*\)/, "\\1)")
    text.split(/;|\bthen\b|\bdo\b|\belse\b|\{|\}/).any? do |command|
      command = command.strip
      next false if command.include?("&&") || command.include?("||")
      next false if command.match?(/\A(if|elif|while|until|!)\b/) || command.start_with?("!")
      command.include?("|")
    end
  end.map { |line| line[:from] }
end

def windows_only?(runs_on)
  labels = Array(runs_on).map(&:to_s)
  !labels.empty? && labels.all? { |label| label.include?("windows") }
end

found = []
files = Dir.glob(".github/workflows/*.{yml,yaml}") + Dir.glob(".github/actions/**/action.{yml,yaml}")
files.sort.each do |file|
  doc = YAML.load_file(file)
  next unless doc.is_a?(Hash)
  top = doc.dig("defaults", "run", "shell")
  steps = []
  (doc["jobs"] || {}).each do |name, job|
    next unless job.is_a?(Hash)
    shell = job.dig("defaults", "run", "shell") || top
    (job["steps"] || []).each { |step| steps << [name, step, shell, windows_only?(job["runs-on"])] }
  end
  # A composite action names a shell for each of its steps itself.
  (doc.dig("runs", "steps") || []).each { |step| steps << ["action", step, nil, false] }
  steps.each do |name, step, shell, windows|
    next unless step.is_a?(Hash) && step["run"].is_a?(String)
    shell = step["shell"] || shell
    next if shell.nil? && windows
    next if shell.is_a?(String) && shell.match?(/\A(pwsh|powershell|cmd|python)/)
    next if pipefail_set?(shell, step["run"])
    lone_pipes(step["run"]).each do |line|
      found << "#{file}: #{name}: \"#{step["name"]}\" pipes with no pipefail (shell: #{shell || "bash -e"}): #{line}"
    end
  end
end
found.each { |line| puts "::error::#{line}" }
puts "#{files.size} workflows and actions: no pipe runs without pipefail" if found.empty?
exit(found.empty? ? 0 : 1)
'
