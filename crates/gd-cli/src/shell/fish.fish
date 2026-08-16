function __gd_hook --on-variable PWD
    command gd hook "$PWD" &>/dev/null &
end

function gd
    switch $argv[1]
        case link unlink config list clean export init doctor setup update boost unboost version help hook '-h' '--help' '-V' '--version'
            command gd $argv
            return $status
        case '-'
            cd -
            return $status
    end

    set -l result (command gd $argv)
    or return $status
    # 用明確的 if：沒有輸出就單純不跳轉，不要讓 test 的 false 變成函式的失敗狀態
    if test -n "$result"
        cd $result
    end
end

complete -c gd -f -a "link unlink config list clean export init doctor help"
