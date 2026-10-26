# profile.d: the shared Applications folder's desktop files, for login shells.
case ":${XDG_DATA_DIRS:-}:" in
    *:/var/lib/elavo-applications/exports/share:*) ;;
    *) XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}:/var/lib/elavo-applications/exports/share"
       export XDG_DATA_DIRS ;;
esac
