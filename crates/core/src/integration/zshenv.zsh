# ale-term shell integration for zsh.
#
# ale-term starts zsh with ZDOTDIR pointing at the folder holding this file,
# so zsh reads it first. It puts ZDOTDIR back, loads your own ~/.zshenv, and
# then adds two hooks that tell the terminal:
#   - the current folder (OSC 7),
#   - where each prompt and command starts and ends (OSC 133).
# Nothing here changes how your shell behaves. Disable it with
# `shell_integration = false` in the ale-term config.

if [[ -n "${ALE_TERM_ORIG_ZDOTDIR+X}" ]]; then
    export ZDOTDIR="$ALE_TERM_ORIG_ZDOTDIR"
    unset ALE_TERM_ORIG_ZDOTDIR
else
    unset ZDOTDIR
fi

# Load the user's own .zshenv, exactly as zsh would have.
if [[ -r "${ZDOTDIR:-$HOME}/.zshenv" ]]; then
    builtin source "${ZDOTDIR:-$HOME}/.zshenv"
fi

if [[ -o interactive && -z "$ALE_TERM_INTEGRATED" ]]; then
    typeset -g ALE_TERM_INTEGRATED=1

    # Percent-encodes $1 into $REPLY so any text survives inside a sequence.
    _ale_term_encode() {
        builtin emulate -L zsh
        builtin local LC_ALL=C text="$1" char hex
        REPLY=""
        builtin local -i i
        for (( i = 1; i <= ${#text}; i++ )); do
            char="$text[i]"
            if [[ "$char" == [a-zA-Z0-9/._~-] ]]; then
                REPLY+="$char"
            else
                builtin printf -v hex '%%%02X' "'$char"
                REPLY+="$hex"
            fi
        done
    }

    _ale_term_precmd() {
        builtin local exit_status=$?
        # The previous command (if any) finished, and here is where we are.
        builtin print -n "\e]133;D;${exit_status}\a"
        _ale_term_encode "$PWD"
        builtin print -n "\e]7;file://${HOST}${REPLY}\a"

        # Mark the prompt. Themes may rebuild PS1 in their own precmd hook,
        # so re-add the marks whenever they are missing, and keep this hook
        # last so it runs after the theme's.
        if [[ "$PS1" != *$'\e]133;A\a'* ]]; then
            PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
        fi
        if [[ "${precmd_functions[-1]}" != _ale_term_precmd ]]; then
            precmd_functions=(${precmd_functions:#_ale_term_precmd} _ale_term_precmd)
        fi
        builtin return $exit_status
    }

    _ale_term_preexec() {
        # $1 is the command line as typed; send at most 400 characters.
        _ale_term_encode "${1[1,400]}"
        builtin print -n "\e]133;C;cmdline_url=${REPLY}\a"
    }

    builtin autoload -Uz add-zsh-hook
    add-zsh-hook precmd _ale_term_precmd
    add-zsh-hook preexec _ale_term_preexec
fi
