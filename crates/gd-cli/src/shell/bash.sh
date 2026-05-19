__gd_bin() { command gd "$@"; }

__gd_hook() {
    __gd_bin hook "$PWD" &>/dev/null &
}

PROMPT_COMMAND="__gd_hook;${PROMPT_COMMAND}"

gd() {
    case "$1" in
        link|unlink|list|clean|export|init|doctor|setup|update|boost|unboost|version|help|hook|"-h"|"--help"|"-V"|"--version")
            __gd_bin "$@"
            return $?
            ;;
        -)
            builtin cd -
            return $?
            ;;
    esac

    local result
    result="$(__gd_bin "$@")" || return $?
    [ -n "$result" ] && builtin cd -- "$result"
}

_gd_complete() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    if [ "$COMP_CWORD" -eq 1 ]; then
        COMPREPLY=( $(compgen -W "link unlink list clean export init doctor help" -- "$cur") )
    fi
}
complete -F _gd_complete gd
