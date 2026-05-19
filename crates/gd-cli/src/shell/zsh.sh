__gd_bin() { command gd "$@"; }

# Hook: record every cd to gd history
__gd_hook() {
    __gd_bin hook "$PWD" &>/dev/null &!
}

autoload -Uz add-zsh-hook
add-zsh-hook chpwd __gd_hook

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

_gd() {
    local -a subcmds
    subcmds=(
        'link:Link an alias to a path'
        'unlink:Remove a link'
        'list:List links and stats'
        'clean:Remove invalid entries'
        'export:Export database as JSON'
        'init:Print shell init script'
        'doctor:Check installation health'
    )
    _describe 'gd commands' subcmds
}
compdef _gd gd
