#!/bin/bash
# check-pipefail.sh [ROOT]: fails if a step of any workflow, or of an action in this repository,
# runs a pipe whose first commands' exit is lost. Under `bash -e` with no `pipefail`, a pipe takes
# its last command's exit, so `fennec-fuzz run … | tee fuzz.txt` passed whatever the fuzzer found,
# and the nightly runs dropped every failure they found from 10-31 to 11-26.
#
# `pipefail` is in force for a step whose shell is `bash`, which GitHub runs as
# `bash --noprofile --norc -eo pipefail {0}`, set on the step, its job's `defaults` or the
# workflow's; for a custom shell naming `pipefail`; and for a script whose first command is
# `set -o pipefail` or a `set` with `-o pipefail`. With no shell named, a step runs under
# `bash -e {0}`, which has none, except on a runner that's only Windows, where PowerShell runs it.
#
# What it finds:
# - With no `pipefail`, a pipe that's a command of its own. A pipe whose exit the script tests
#   itself isn't: a condition of `if`, `elif`, `while` or `until`, one after `!`, and one in an
#   `&&` or `||` list whose last command only reads quietly, as `grep -q`, `cmp -s` or
#   `diff --quiet`. With `pipefail` there, `cmd | grep -q` would fail when grep stops `cmd` early,
#   so it isn't asked for. One like `cmd | tee out || status=$?` is found: `status` gets the exit
#   of tee, and the exit of cmd is lost.
# - A pipe inside `$( )` or backquotes, unless the substitution is all of an assignment,
#   `name=$(a | b)`, and `pipefail` is in force: anywhere else, as `[ "$(a | b)" = x ]`,
#   `echo "$(a | b)"`, an argument or `local name=$(a | b)`, its exit is lost even with it.
#   `./checked | sort | sha256sum` in a test of what it printed hid a crash that way.
# - A pipe inside `<( )` or `>( )`, whose exit is always lost.
# A `|` inside single quotes, a `${{ }}`, a comment or a `case` pattern isn't a pipe, nor is `||`.
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

# Where the bracket opened at `open` closes, past quotes and brackets inside it.
def close(text, open)
  depth, at = 0, open
  while at < text.length
    case text[at]
    when "\\" then at += 1
    when "\x27" then at = text.index("\x27", at + 1) || text.length
    when "\"" then at = close_quote(text, at)
    when "(" then depth += 1
    when ")"
      depth -= 1
      return at if depth.zero?
    end
    at += 1
  end
  text.length
end

# Where the double quote opened at `open` closes, past substitutions inside it.
def close_quote(text, open)
  at = open + 1
  while at < text.length
    case text[at]
    when "\\" then at += 1
    when "\"" then return at
    when "$" then at = close(text, at + 1) if text[at + 1] == "("
    when "`" then at = text.index("`", at + 1) || text.length
    end
    at += 1
  end
  text.length
end

# A line of shell as it runs at its own level: quoted text and arithmetic left out, comments cut,
# and each substitution as a token `SUBn`; and the substitutions, each its kind and its text.
def scan(text)
  flat, subs, at, quoted = +"", [], 0, false
  while at < text.length
    c = text[at]
    if !quoted && c == "#" && (at.zero? || text[at - 1].match?(/\s/))
      break
    elsif c == "\\"
      flat << (quoted ? "" : text[at, 2])
      at += 2
      next
    elsif !quoted && c == "\x27"
      at = (text.index("\x27", at + 1) || text.length) + 1
      flat << "\x27\x27"
      next
    elsif c == "\""
      quoted = !quoted
      flat << c
    elsif text[at, 3] == "$(("
      at = close(text, at + 1) + 1
      flat << "0"
      next
    elsif text[at, 2] == "$(" || (!quoted && ["<(", ">("].include?(text[at, 2]))
      shut = close(text, at + 1)
      subs << [text[at, 2], text[(at + 2)...shut].to_s]
      flat << "SUB#{subs.size - 1}"
      at = shut + 1
      next
    elsif c == "`"
      shut = text.index("`", at + 1) || text.length
      subs << ["`", text[(at + 1)...shut].to_s]
      flat << "SUB#{subs.size - 1}"
      at = shut + 1
      next
    elsif !quoted
      flat << c
    end
    at += 1
  end
  [flat, subs]
end

# The commands of shell at its own level, a `case` pattern as `a | b)` left out.
def commands(flat)
  flat.gsub(/(\bin\s+|\A\s*|;;\s*)(?:[^()|;\s]*\s*\|\s*)+[^()|;\s]*\)/, "\\1)")
      .split(/;|\bthen\b|\bdo\b|\belse\b|\{|\}/).map(&:strip)
end

# Whether the last command of a pipe only reads, and says what it found by its exit alone.
def quiet_tail?(pipe)
  last = pipe.split("|").last.to_s.strip
  last.match?(/\A(\S*\/)?(grep|egrep|fgrep|rg|cmp|diff)\b/) &&
    last.match?(/\s(-[A-Za-z]*[qs][A-Za-z]*|--quiet|--silent)\b/)
end

def piped?(flat)
  commands(flat).any? { |command| command.gsub("||", "").include?("|") }
end

# The pipes in a line of shell, and those in what it substitutes, whose exit is lost: each said
# as what loses it.
def lost(text, pipefail, inside = false)
  flat, subs = scan(text)
  said = []
  unless pipefail || inside
    lone = commands(flat).any? do |command|
      next false if command.match?(/\A(if|elif|while|until)\b/) || command.start_with?("!")
      # In an `&&` or `||` list a pipe is tested only when its last command reads quietly, as
      # `cmd | grep -q x || …`, where `pipefail` would fail it when grep stops `cmd` early. One
      # like `cmd | tee out || status=$?` takes the exit of tee, and loses the exit of cmd.
      parts = command.split(/&&|\|\|/, -1)
      next command.include?("|") if parts.size == 1
      parts.any? { |part| part.include?("|") && !quiet_tail?(part) }
    end
    said << "pipes with no pipefail" if lone
  end
  subs.each_with_index do |(kind, inner), number|
    if piped?(scan(inner).first)
      token = "SUB#{number}"
      # An assignment takes the exit of the last substitution in what it assigns.
      whole = commands(flat).any? do |command|
        command.sub(/\s*(&&|\|\|).*\z/, "").match?(/\A[A-Za-z_][A-Za-z0-9_]*=\S*#{token}(?!\d)(?!\S*SUB\d)\S*\z/)
      end
      if kind == "<(" || kind == ">("
        said << "pipes inside #{kind} ), whose exit is always lost"
      elsif !whole
        said << "pipes inside $( ) within another command, whose exit is lost even with pipefail"
      elsif !pipefail
        said << "pipes inside an assignment with no pipefail"
      end
    end
    said.concat(lost(inner, pipefail, true))
  end
  said.uniq
end

# Each line of a script, with those carried on by `\` or a closing `|` joined to the next.
def logical_lines(script)
  lines = []
  script.each_line do |line|
    text = line.chomp.gsub(/\$\{\{.*?\}\}/, "")
    if !lines.empty? && lines.last[:open]
      lines.last[:text] += " " + text
    else
      lines << { text: text, from: line.strip }
    end
    ending = scan(text).first.rstrip
    lines.last[:open] = ending.end_with?("\\", "|") && !ending.end_with?("||")
  end
  lines
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
    pipefail = pipefail_set?(shell, step["run"])
    logical_lines(step["run"]).each do |line|
      lost(line[:text], pipefail).each do |what|
        found << "#{file}: #{name}: \"#{step["name"]}\" #{what} (shell: #{shell || "bash -e"}): #{line[:from]}"
      end
    end
  end
end
found.each { |line| puts "::error::#{line}" }
puts "#{files.size} workflows and actions: no pipe loses its exit" if found.empty?
exit(found.empty? ? 0 : 1)
'
