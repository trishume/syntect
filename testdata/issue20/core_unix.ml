(* Core_unix wraps the standard unix functions with an exception handler that inserts an
   informative string in the third field of Unix_error.  The problem with the standard
   Unix_error that gets raised is that it doesn't include information about the arguments
   to the function that failed. *)
#import "config.h"

open! Import

module Time_ns = Core_kernel.Core_kernel_private.Time_ns_alternate_sexp

(* 4.06 adds a [?cloexec] argument to all functions. It cannot be ignored for the
   functions with labels, so to keep the code compatible with all supported versions of
   OCaml, we sometimes use the non-labelled versions. *)
module UnixNoLabels = Unix
module Unix = UnixLabels

let ( ^/ ) = Core_filename.concat

let atom x = Sexp.Atom x
let list x = Sexp.List x

let record l =
  list (List.map l ~f:(fun (name, value) -> list [atom name; value]))
;;

(* No need to include a counter here. It just doesn't make sense to think we are
going to be receiving a steady stream of interrupts.
   Glibc's macro doesn't have a counter either.
*)
let rec retry_until_no_eintr f =
  try
    f ()
  with Unix.Unix_error (EINTR, _, _) ->
    retry_until_no_eintr f

(* This wrapper improves the content of the Unix_error exception raised by the standard
   library (by including a sexp of the function arguments), and it optionally restarts
   syscalls on EINTR. *)
let improve ?(restart = false) f make_arg_sexps =
  try
    if restart then retry_until_no_eintr f else f ()
  with
  | Unix.Unix_error (e, s, _) ->
    let buf = Buffer.create 100 in
    let fmt = Format.formatter_of_buffer buf in
    Format.pp_set_margin fmt 10000;
    Sexp.pp_hum fmt (record (make_arg_sexps ()));
    Format.pp_print_flush fmt ();
    let arg_str = Buffer.contents buf in
    raise (Unix.Unix_error (e, s, arg_str))
;;

module File_descr = struct
  module M = struct
    type t = Unix.file_descr
    external to_int : t -> int = "%identity"
    external of_int : int -> t = "%identity"
    let of_string string = of_int (Int.of_string string)
    let to_string t = Int.to_string (to_int t)
    let sexp_of_t t = Int.sexp_of_t (to_int t)
    let t_of_sexp sexp = of_int (Int.t_of_sexp sexp)
    let hash t = Int.hash (to_int t)
    let compare t1 t2 = Int.compare (to_int t1) (to_int t2)
  end
  include M
  include (Hashable.Make (M))
  include (Binable.Of_stringable (M))

  (* Given that [to_int] and [of_int] are set to "%identity", this is considerably more
     direct.  It's unfortunate, but despite [Caml.Unix] using [type t = int] in the
     implementation, [Unix.file_descr] is abstract and cannot be tagged [@@immediate]. *)
  let equal (t1 : t) t2 = phys_equal t1 t2
end

let sprintf = Printf.sprintf

external sync : unit -> unit = "unix_sync"
external fsync : Unix.file_descr -> unit = "unix_fsync"
external fdatasync : Unix.file_descr -> unit = "unix_fdatasync"

external dirfd : Unix.dir_handle -> File_descr.t = "unix_dirfd"

external readdir_ino
  : Unix.dir_handle -> string * nativeint = "unix_readdir_ino_stub"

let readdir_ino_opt dh =
  match readdir_ino dh with
  | entry                 -> Some entry
  | exception End_of_file -> None

external unsetenv : string -> unit = "unix_unsetenv"

external exit_immediately : int -> _ = "caml_sys_exit"

external unsafe_read_assume_fd_is_nonblocking
  : File_descr.t -> string -> pos : int -> len : int -> int
  = "unix_read_assume_fd_is_nonblocking_stub"

let check_string_args ~loc str ~pos ~len =
  if pos < 0 then invalid_arg (loc ^ ": pos < 0");
  if len < 0 then invalid_arg (loc ^ ": len < 0");
  let str_len = String.length str in
  if str_len < pos + len then
    invalid_arg (Printf.sprintf "Unix_ext.%s: length(str) < pos + len" loc)

let get_opt_pos ~loc = function
  | Some pos ->
    if pos < 0 then invalid_arg (Printf.sprintf "Unix_ext.%s: pos < 0" loc);
    pos
  | None -> 0

let get_opt_len str ~pos = function
  | Some len -> len
  | None -> String.length str - pos

let read_assume_fd_is_nonblocking fd ?pos ?len buf =
  let loc = "read_assume_fd_is_nonblocking" in
  let pos = get_opt_pos ~loc pos in
  let len = get_opt_len buf ~pos len in
  check_string_args ~loc buf ~pos ~len;
  unsafe_read_assume_fd_is_nonblocking fd buf ~pos ~len
;;

external unsafe_write_assume_fd_is_nonblocking
  : File_descr.t -> string -> pos : int -> len : int -> int
  = "unix_write_assume_fd_is_nonblocking_stub"
;;

let write_assume_fd_is_nonblocking fd ?pos ?len buf =
  let loc = "write_assume_fd_is_nonblocking" in
  let pos = get_opt_pos ~loc pos in
  let len = get_opt_len buf ~pos len in
  check_string_args ~loc buf ~pos ~len;
  unsafe_write_assume_fd_is_nonblocking fd buf ~pos ~len
;;

(* Filesystem functions *)

external mknod
  : string -> Unix.file_kind -> int -> int -> int -> unit = "unix_mknod_stub"

let mknod
    ?(file_kind = Unix.S_REG) ?(perm = 0o600) ?(major = 0) ?(minor = 0)
    pathname =
  mknod pathname file_kind perm major minor

(* Resource limits *)

module RLimit = struct
  type limit = Limit of int64 | Infinity [@@deriving sexp]
  type t = { cur : limit; max : limit } [@@deriving sexp]

  type resource =
    | Core_file_size
    | Cpu_seconds
    | Data_segment
    | File_size
    | Num_file_descriptors
    | Stack
    | Virtual_memory
    | Nice
  [@@deriving sexp] ;;

  let core_file_size       = Core_file_size
  let cpu_seconds          = Cpu_seconds
  let data_segment         = Data_segment
  let file_size            = File_size
  let num_file_descriptors = Num_file_descriptors
  let stack                = Stack
  let virtual_memory       =
#ifdef JSC_RLIMIT_AS
      Ok Virtual_memory
#else
      Or_error.unimplemented "RLIMIT_AS is not supported on this system"
#endif
  let nice                 =
#ifdef JSC_RLIMIT_NICE
      Ok Nice
#else
      Or_error.unimplemented "RLIMIT_NICE is not supported on this system"
#endif

  let resource_of_sexp sexp =
    match resource_of_sexp sexp with
    | Nice ->
      begin
        match nice with
        | Ok resource -> resource
        | Error error -> of_sexp_error (Error.to_string_hum error) sexp
      end
    | Core_file_size | Cpu_seconds | Data_segment | File_size
    | Num_file_descriptors | Stack | Virtual_memory as resource ->
      resource

  external get : resource -> t = "unix_getrlimit"
  external set : resource -> t -> unit = "unix_setrlimit"

  let get resource =
    improve (fun () -> get resource)
      (fun () -> [("resource", sexp_of_resource resource)])
  ;;

  let set resource t =
    improve (fun () -> set resource t)
      (fun () ->  [("resource", sexp_of_resource resource);
                   ("limit", sexp_of_t t);
                  ])
  ;;
end


(* Resource usage *)

module Resource_usage = struct
  type t = {
    utime : float;
    stime : float;
    maxrss : int64;
    ixrss : int64;
    idrss : int64;
    isrss : int64;
    minflt : int64;
    majflt : int64;
    nswap : int64;
    inblock : int64;
    oublock : int64;
    msgsnd : int64;
    msgrcv : int64;
    nsignals : int64;
    nvcsw : int64;
    nivcsw : int64;
  }
  [@@deriving sexp, fields]

  external getrusage : int -> t = "unix_getrusage"

  let get who = getrusage (match who with `Self -> 0 | `Children -> 1)

  let add t1 t2 = {
    utime = t1.utime +. t2.utime;
    stime = t1.stime +. t2.stime;
    maxrss = Int64.(+) t1.maxrss t2.maxrss;
    ixrss = Int64.(+) t1.ixrss t2.ixrss;
    idrss = Int64.(+) t1.idrss t2.idrss;
    isrss = Int64.(+) t1.isrss t2.isrss;
    minflt = Int64.(+) t1.minflt t2.minflt;
    majflt = Int64.(+) t1.majflt t2.majflt;
    nswap = Int64.(+) t1.nswap t2.nswap;
    inblock = Int64.(+) t1.inblock t2.inblock;
    oublock = Int64.(+) t1.oublock t2.oublock;
    msgsnd = Int64.(+) t1.msgsnd t2.msgsnd;
    msgrcv = Int64.(+) t1.msgrcv t2.msgrcv;
    nsignals = Int64.(+) t1.nsignals t2.nsignals;
    nvcsw = Int64.(+) t1.nvcsw t2.nvcsw;
    nivcsw = Int64.(+) t1.nivcsw t2.nivcsw;
  }
end


(* System configuration *)
type sysconf =
  | ARG_MAX
  | CHILD_MAX
  | HOST_NAME_MAX
  | LOGIN_NAME_MAX
  | OPEN_MAX
  | PAGESIZE
  | RE_DUP_MAX
  | STREAM_MAX
  | SYMLOOP_MAX
  | TTY_NAME_MAX
  | TZNAME_MAX
  | POSIX_VERSION
  | PHYS_PAGES
  | AVPHYS_PAGES
  | IOV_MAX
[@@deriving sexp]

external sysconf : sysconf -> int64 = "unix_sysconf"


(* I/O vectors *)

module IOVec = struct
  open Bigarray

  (* NOTE: DO NOT CHANGE THE MEMORY LAYOUT OF THIS TYPE!!! *)
  type 'buf t =
    {
      buf : 'buf;
      pos : int;
      len : int;
    }
  [@@deriving sexp]

  type 'buf kind = 'buf

  type bigstring = (char, int8_unsigned_elt, c_layout) Array1.t

  let string_kind = ""
  let bigstring_kind = Array1.create Bigarray.char c_layout 0

  let empty kind =
    {
      buf = kind;
      pos = 0;
      len = 0;
    }

  let get_iovec loc ?pos ?len true_len buf =
    let pos =
      match pos with
      | None -> 0
      | Some pos ->
          if pos < 0 then invalid_arg (loc ^ ": pos < 0");
          pos
    in
    let len =
      match len with
      | None -> true_len
      | Some len ->
          if len < 0 then invalid_arg (loc ^ ": len < 0");
          len
    in
    if pos + len > true_len then invalid_arg (loc ^ ": pos + len > length buf");
    {
      buf = buf;
      pos = pos;
      len = len;
    }
  ;;

  let of_string ?pos ?len str =
    let str_len = String.length str in
    get_iovec "IOVec.of_string" ?pos ?len str_len str
  ;;

  let of_bigstring ?pos ?len bstr =
    let bstr_len = Array1.dim bstr in
    get_iovec "IOVec.of_bigstring" ?pos ?len bstr_len bstr
  ;;

  let drop iovec n =
    if n > iovec.len then failwith "IOVec.drop: n > length iovec"
    else
      {
        buf = iovec.buf;
        pos = iovec.pos + n;
        len = iovec.len - n;
      }
  ;;

  let max_iovecs =
    let n64 = sysconf IOV_MAX in
    if n64 > Int64.of_int Array.max_length then Array.max_length
    else Int64.to_int_exn n64
  ;;
end

let get_iovec_count loc iovecs = function
  | None -> Array.length iovecs
  | Some count ->
      if count < 0 then invalid_arg (loc ^ ": count < 0");
      let n_iovecs = Array.length iovecs in
      if count > n_iovecs then invalid_arg (loc ^ ": count > n_iovecs");
      count
;;

external unsafe_writev_assume_fd_is_nonblocking
  : File_descr.t -> string IOVec.t array -> int -> int
  = "unix_writev_assume_fd_is_nonblocking_stub"
;;

let writev_assume_fd_is_nonblocking fd ?count iovecs =
  let count = get_iovec_count "writev_assume_fd_is_nonblocking" iovecs count in
  unsafe_writev_assume_fd_is_nonblocking fd iovecs count
;;

external unsafe_writev
  : File_descr.t -> string IOVec.t array -> int -> int = "unix_writev_stub"
;;

let writev fd ?count iovecs =
  let count = get_iovec_count "writev" iovecs count in
  unsafe_writev fd iovecs count
;;

external pselect
  :    File_descr.t list
    -> File_descr.t list
    -> File_descr.t list
    -> float
    -> int list
    -> File_descr.t list * File_descr.t list * File_descr.t list
  = "unix_pselect_stub"
;;

(* Temporary file and directory creation *)
external mkstemp : string -> string * File_descr.t = "unix_mkstemp"
external mkdtemp : string -> string = "unix_mkdtemp"

(* Signal handling *)

external abort : unit -> 'a = "unix_abort" [@@noalloc]

(* User id, group id management *)

external initgroups : string -> int -> unit = "unix_initgroups"

external getgrouplist : string -> int -> int array = "unix_getgrouplist"

(** Globbing and shell word expansion *)

module Fnmatch_flags = struct
  type _flag = [
    | `No_escape
    | `Pathname
    | `Period
    | `File_name
    | `Leading_dir
    | `Casefold
  ]
  [@@deriving sexp]

  let flag_to_internal = function
    | `No_escape -> 0
    | `Pathname -> 1
    | `Period -> 2
    | `File_name -> 3
    | `Leading_dir -> 4
    | `Casefold -> 5
  ;;

  type t = int32 [@@deriving sexp]

  external internal_make : int array -> t = "unix_fnmatch_make_flags"

  let make = function
    | None | Some [] -> Int32.zero
    | Some flags -> internal_make (Array.map ~f:flag_to_internal (Array.of_list flags))
  ;;
end

external fnmatch
  : Fnmatch_flags.t -> pat : string -> string -> bool = "unix_fnmatch"
;;

let fnmatch ?flags ~pat fname = fnmatch (Fnmatch_flags.make flags) ~pat fname

#ifdef JSC_WORDEXP

module Wordexp_flags = struct
  type _flag = [ `No_cmd | `Show_err | `Undef ] [@@deriving sexp]

  let flag_to_internal = function
    | `No_cmd -> 0
    | `Show_err -> 1
    | `Undef -> 2
  ;;

  type t = int32 [@@deriving sexp]

  external internal_make : int array -> t = "unix_wordexp_make_flags"

  let make = function
    | None | Some [] -> Int32.zero
    | Some flags -> internal_make (Array.map ~f:flag_to_internal (Array.of_list flags))
  ;;
end

external wordexp : Wordexp_flags.t -> string -> string array = "unix_wordexp"

let wordexp = Ok (fun ?flags str -> wordexp (Wordexp_flags.make flags) str)

#else

let wordexp = Or_error.unimplemented "Unix.wordexp"

#endif

(* System information *)

module Utsname = struct
  type t =
    { sysname: string;
      nodename: string;
      release: string;
      version: string;
      machine: string;
    }
  [@@deriving fields, sexp]
end

external uname : unit -> Utsname.t = "unix_uname"

module Scheduler = struct
  module Policy = struct
    type t = [ `Fifo | `Round_robin | `Other ] [@@deriving sexp]

    module Ordered = struct
      type t = Fifo | Round_robin | Other [@@deriving sexp]
      let create = function
        | `Fifo -> Fifo
        | `Round_robin -> Round_robin
        | `Other -> Other
      ;;
    end
  end

  external set
    : pid : int -> policy : Policy.Ordered.t -> priority : int -> unit
    = "unix_sched_setscheduler"
  ;;

  let set ~pid ~policy ~priority =
    let pid =
      match pid with
      | None -> 0
      | Some pid -> Pid.to_int pid
    in
    set ~pid ~policy:(Policy.Ordered.create policy) ~priority
  ;;
end

module Priority = struct
  external nice : int -> int = "unix_nice"
end

module Mman = struct
  module Mcl_flags = struct
    type t =
      (* Do not change the ordering of this type without also
         changing the C stub. *)
      | Current
      | Future
    [@@deriving sexp]
  end
  external unix_mlockall   : Mcl_flags.t array -> unit = "unix_mlockall" ;;
  external unix_munlockall : unit -> unit = "unix_munlockall" ;;

  let mlockall flags = unix_mlockall (List.to_array flags) ;;
  let munlockall = unix_munlockall ;;
end ;;

let dirname_r filename = ("dirname", atom filename)
let filename_r filename = ("filename", atom filename)
let file_perm_r perm = ("perm", atom (Printf.sprintf "0o%o" perm))
let len_r len = ("len", Int.sexp_of_t len)
let uid_r uid = ("uid", Int.sexp_of_t uid)
let gid_r gid = ("gid", Int.sexp_of_t gid)
let fd_r fd = ("fd", File_descr.sexp_of_t fd)
let dir_handle_r handle =
  let fd =
    try File_descr.sexp_of_t (dirfd handle)
    with _ -> Int.sexp_of_t (-1)
  in
  ("dir_handle", fd)
;;

let unary ?restart make_r f =
  ();
  fun x -> improve ?restart (fun () -> f x) (fun () -> [make_r x])
;;

let unary_fd ?restart f = unary ?restart fd_r f
let unary_filename ?restart f = unary ?restart filename_r f
let unary_dirname ?restart f = unary ?restart dirname_r f
let unary_dir_handle ?restart f = unary ?restart dir_handle_r f

include Unix_error

module Syscall_result = Syscall_result

exception Unix_error = Unix.Unix_error

external unix_error : int -> string -> string -> _ = "unix_error_stub"
let error_message = Unix.error_message
let handle_unix_error f = Unix.handle_unix_error f ()
let environment = Unix.environment

module Error = struct
  type t = Unix.error =
    | E2BIG               (** Argument list too long *)
    | EACCES              (** Permission denied *)
    | EAGAIN              (** Resource temporarily unavailable; try again *)
    | EBADF               (** Bad file descriptor *)
    | EBUSY               (** Resource unavailable *)
    | ECHILD              (** No child process *)
    | EDEADLK             (** Resource deadlock would occur *)
    | EDOM                (** Domain error for math functions, etc. *)
    | EEXIST              (** File exists *)
    | EFAULT              (** Bad address *)
    | EFBIG               (** File too large *)
    | EINTR               (** Function interrupted by signal *)
    | EINVAL              (** Invalid argument *)
    | EIO                 (** Hardware I/O error *)
    | EISDIR              (** Is a directory *)
    | EMFILE              (** Too many open files by the process *)
    | EMLINK              (** Too many links *)
    | ENAMETOOLONG        (** Filename too long *)
    | ENFILE              (** Too many open files in the system *)
    | ENODEV              (** No such device *)
    | ENOENT              (** No such file or directory *)
    | ENOEXEC             (** Not an executable file *)
    | ENOLCK              (** No locks available *)
    | ENOMEM              (** Not enough memory *)
    | ENOSPC              (** No space left on device *)
    | ENOSYS              (** Function not supported *)
    | ENOTDIR             (** Not a directory *)
    | ENOTEMPTY           (** Directory not empty *)
    | ENOTTY              (** Inappropriate I/O control operation *)
    | ENXIO               (** No such device or address *)
    | EPERM               (** Operation not permitted *)
    | EPIPE               (** Broken pipe *)
    | ERANGE              (** Result too large *)
    | EROFS               (** Read-only file system *)
    | ESPIPE              (** Invalid seek e.g. on a pipe *)
    | ESRCH               (** No such process *)
    | EXDEV               (** Invalid link *)

    | EWOULDBLOCK         (** Operation would block *)
    | EINPROGRESS         (** Operation now in progress *)
    | EALREADY            (** Operation already in progress *)
    | ENOTSOCK            (** Socket operation on non-socket *)
    | EDESTADDRREQ        (** Destination address required *)
    | EMSGSIZE            (** Message too long *)
    | EPROTOTYPE          (** Protocol wrong type for socket *)
    | ENOPROTOOPT         (** Protocol not available *)
    | EPROTONOSUPPORT     (** Protocol not supported *)
    | ESOCKTNOSUPPORT     (** Socket type not supported *)
    | EOPNOTSUPP          (** Operation not supported on socket *)
    | EPFNOSUPPORT        (** Protocol family not supported *)
    | EAFNOSUPPORT        (** Address family not supported by protocol family *)
    | EADDRINUSE          (** Address already in use *)
    | EADDRNOTAVAIL       (** Can't assign requested address *)
    | ENETDOWN            (** Network is down *)
    | ENETUNREACH         (** Network is unreachable *)
    | ENETRESET           (** Network dropped connection on reset *)
    | ECONNABORTED        (** Software caused connection abort *)
    | ECONNRESET          (** Connection reset by peer *)
    | ENOBUFS             (** No buffer space available *)
    | EISCONN             (** Socket is already connected *)
    | ENOTCONN            (** Socket is not connected *)
    | ESHUTDOWN           (** Can't send after socket shutdown *)
    | ETOOMANYREFS        (** Too many references: can't splice *)
    | ETIMEDOUT           (** Connection timed out *)
    | ECONNREFUSED        (** Connection refused *)
    | EHOSTDOWN           (** Host is down *)
    | EHOSTUNREACH        (** No route to host *)
    | ELOOP               (** Too many levels of symbolic links *)
    | EOVERFLOW           (** File size or position not representable *)

    | EUNKNOWNERR of int  (** Unknown error *)
  [@@deriving sexp]

  let of_system_int ~errno = Unix_error.of_errno errno

  let message = Unix.error_message
end

let putenv ~key ~data =
  improve (fun () -> Unix.putenv key data)
    (fun () -> [("key", atom key); ("data", atom data)])
;;

let unsetenv name =
  (* The C unsetenv has only one error: EINVAL if name contains an '='
     character. C strings are null terminated though so '\000' is also invalid.
  *)
  if String.contains name '\000' then
    raise (Unix_error (EINVAL,"unsetenv",name));
  unsetenv name
;;

type process_status = Unix.process_status =
| WEXITED of int
| WSIGNALED of int
| WSTOPPED of int
[@@deriving sexp]

module Exit = struct
  type error = [ `Exit_non_zero of int ] [@@deriving compare, sexp]

  type t = (unit, error) Result.t [@@deriving compare, sexp]

  let to_string_hum = function
    | Ok () -> "exited normally"
    | Error (`Exit_non_zero i) -> sprintf "exited with code %d" i
  ;;

  let code = function
    | Ok () -> 0
    | Error (`Exit_non_zero i) -> i
  ;;

  exception Exit_code_must_be_nonnegative of int [@@deriving sexp]

  let of_code code =
    if code < 0 then
      raise (Exit_code_must_be_nonnegative code)
    else if code = 0 then
      Ok ()
    else
      Error (`Exit_non_zero code)
  ;;

  let or_error = function
    | Ok _ as ok  -> ok
    | Error error -> Or_error.error "Unix.Exit" error sexp_of_error
  ;;
end

module Exit_or_signal = struct
  type error = [ Exit.error | `Signal of Signal.t ] [@@deriving compare, sexp]

  type t = (unit, error) Result.t [@@deriving compare, sexp]

  let to_string_hum = function
    | Ok () | Error #Exit.error as e -> Exit.to_string_hum e
    | Error (`Signal s) ->
      sprintf "died after receiving %s (signal number %d)"
        (Signal.to_string s) (Signal.to_system_int s)
  ;;

  exception Of_unix_got_invalid_status of process_status [@@deriving sexp]

  let of_unix = function
    | WEXITED i -> if i = 0 then Ok () else Error (`Exit_non_zero i)
    | WSIGNALED i -> Error (`Signal (Signal.of_caml_int i))
    | WSTOPPED _ as status -> raise (Of_unix_got_invalid_status status)
  ;;

  let or_error = function
    | Ok _ as ok  -> ok
    | Error error -> Or_error.error "Unix.Exit_or_signal" error sexp_of_error
  ;;
end

module Exit_or_signal_or_stop = struct
  type error = [ Exit_or_signal.error | `Stop of Signal.t ] [@@deriving sexp]

  type t = (unit, error) Result.t [@@deriving sexp]

  let to_string_hum = function
    | Ok () | Error #Exit_or_signal.error as e -> Exit_or_signal.to_string_hum e
    | Error (`Stop s) ->
        sprintf "stopped by %s (signal number %d)"
          (Signal.to_string s) (Signal.to_system_int s)
  ;;

  let of_unix = function
    | WEXITED i -> if i = 0 then Ok () else Error (`Exit_non_zero i)
    | WSIGNALED i -> Error (`Signal (Signal.of_caml_int i))
    | WSTOPPED i -> Error (`Stop (Signal.of_caml_int i))
  ;;

  let or_error = function
    | Ok _ as ok  -> ok
    | Error error -> Or_error.error "Unix.Exit_or_signal_or_stop" error sexp_of_error
  ;;
end

let prog_r prog = ("prog", atom prog)
let args_r argv = ("argv", sexp_of_array atom argv)
let env_r env = ("env", sexp_of_array atom env)

let execv ~prog ~argv =
  improve (fun () -> Unix.execv ~prog ~args:argv)
    (fun () -> [prog_r prog; args_r argv])
;;

let execve ~prog ~argv ~env =
  improve (fun () -> Unix.execve ~prog ~args:argv ~env)
    (fun () -> [prog_r prog; args_r argv; env_r env])
;;

let execvp ~prog ~argv =
  improve (fun () -> Unix.execvp ~prog ~args:argv)
    (fun () -> [prog_r prog; args_r argv])
;;

let execvpe ~prog ~argv ~env =
  improve (fun () -> Unix.execvpe ~prog ~args:argv ~env)
    (fun () -> [prog_r prog; args_r argv; env_r env])
;;

type env =
  [ `Replace of (string * string) list
  | `Extend of (string * string) list
  | `Replace_raw of string list
  ]
[@@deriving sexp]

let env_map env =
  let current, env =
    match env with
    | `Replace env -> [], env
    | `Extend env ->
      let current =
        List.map (Array.to_list (Unix.environment ()))
          ~f:(fun s -> String.lsplit2_exn s ~on:'=')
      in
      current, env
  in
  List.fold_left (current @ env) ~init:String.Map.empty
    ~f:(fun map (key, data) -> Map.add map ~key ~data)
;;

let env_assignments env =
  match env with
  | `Replace_raw env -> env
  | `Replace _
  | `Extend  _ as env ->
    Map.fold (env_map env) ~init:[]
      ~f:(fun ~key ~data acc -> (key ^ "=" ^ data) :: acc)
;;

(* Linux allows arbitrary strings to be passed as environment entries: they don't even
   have to contain the '=' character.
   You can also pass the same variable multiple times and that's somehow fine. Most
   programs (in particular, C stdlib) will use the first entry if there are multiple. *)
let lookup_raw env key =
  List.find_map env ~f:(fun x ->
    match String.lsplit2 x ~on:'=' with
    | None -> None
    | Some (key2, value) ->
      if String.(=) key key2
      then Some value
      else None)
;;

let env_lookup env key =
  match env with
  | `Extend  _
  | `Replace _ as env -> Map.find (env_map env) key
  | `Replace_raw raw_env -> lookup_raw raw_env key
;;

let exec ~prog ~argv ?(use_path = true) ?env () =
  let argv = Array.of_list argv in
  let env = Option.map env ~f:(Fn.compose Array.of_list env_assignments) in
  match use_path, env with
  | false, None -> execv ~prog ~argv
  | false, Some env -> execve ~prog ~argv ~env
  | true, None -> execvp ~prog ~argv
  | true, Some env -> execvpe ~prog ~argv ~env
;;

exception Fork_returned_negative_result of int [@@deriving sexp]

let fork () =
  let pid = Unix.fork () in
  if pid < 0 then
    raise (Fork_returned_negative_result pid)
  else if pid = 0 then
    `In_the_child
  else
    `In_the_parent (Pid.of_int pid)
;;

(* Same as [Pervasives.exit] but does not run at_exit handlers *)
external sys_exit : int -> 'a = "caml_sys_exit"

let fork_exec ~prog ~argv ?use_path ?env () =
  match fork () with
  | `In_the_child ->
    never_returns (
      try exec ~prog ~argv ?use_path ?env ()
      with _ -> sys_exit 127
    )
  | `In_the_parent pid -> pid
;;

type wait_flag =
  Unix.wait_flag =
| WNOHANG
| WUNTRACED
[@@deriving sexp]

type wait_on =
  [ `Any
  | `My_group
  | `Group of Pid.t
  | `Pid of Pid.t
  ]
[@@deriving sexp]

type mode = wait_flag list [@@deriving sexp_of]
type _t = mode

type waitpid_result = (Pid.t * Exit_or_signal_or_stop.t) option [@@deriving sexp_of]

let wait_gen
    ~mode
    (type a) (f : waitpid_result -> a option)
    ~restart
    wait_on : a =
  let pid =
    match wait_on with
    | `Any -> -1
    | `Group pid -> - (Pid.to_int pid)
    | `My_group -> 0
    | `Pid pid -> Pid.to_int pid
  in
  let (pid, status) =
    improve ~restart
      (fun () ->
        let x, ps = Unix.waitpid ~mode pid in
        (x, Exit_or_signal_or_stop.of_unix ps))
      (fun () ->
        [("mode", sexp_of_list sexp_of_wait_flag mode);
         ("pid", Int.sexp_of_t pid)])
  in
  let waitpid_result =
    if pid = 0 then
      None
    else begin
      let pid = Pid.of_int pid in
      Some (pid, status)
    end
  in
  match f waitpid_result with
  | Some a -> a
  | None ->
    failwiths "waitpid syscall returned invalid result for mode"
      (pid, mode, waitpid_result)
      ([%sexp_of: int * mode * waitpid_result])
;;

let wait ?(restart=true) pid =
  let f = function
    | Some ((_, (Ok _ | Error #Exit_or_signal.error)) as x) -> Some x
    | _ -> None
  in
  wait_gen ~restart ~mode:[] f pid
;;

let wait_nohang pid =
  let f = function
    | None | Some ((_, (Ok _ | Error #Exit_or_signal.error))) as x -> Some x
    | _ -> None
  in
  wait_gen ~mode:[WNOHANG] ~restart:true f pid
;;

let wait_untraced ?(restart=true) pid =
  wait_gen ~restart ~mode:[WUNTRACED] Fn.id pid

let wait_nohang_untraced pid =
  wait_gen ~mode:[WNOHANG; WUNTRACED] Option.some ~restart:true pid

let waitpid pid =
  let (pid', exit_or_signal) = wait (`Pid pid) in
  assert (pid = pid');
  exit_or_signal;
;;

let waitpid_exn pid =
  let exit_or_signal = waitpid pid in
  if Result.is_error exit_or_signal then
    failwiths "child process didn't exit with status zero"
      (`Child_pid pid, exit_or_signal)
      ([%sexp_of: [ `Child_pid of Pid.t ] * Exit_or_signal.t])
;;

let system s =
  improve (fun () -> Exit_or_signal.of_unix (Unix.system s))
    (fun () -> [("command", atom s)])
;;

let getpid () = Pid.of_int (Unix.getpid ())

let getppid () =
  match Unix.getppid () with
  | x when x < 1 -> None
  | x -> Some (Pid.of_int x)

let getppid_exn () =
  Option.value_exn ~message:"You don't have a parent process"
    (getppid ())

module Thread_id = Int

#if JSC_THREAD_ID_METHOD > 0
external gettid : unit -> Thread_id.t = "unix_gettid"
let gettid = Ok gettid
#else
let gettid = Or_error.unimplemented "gettid is not supported on this system"
#endif

let nice i =
  improve (fun () -> Unix.nice i)
    (fun () -> [("priority", Int.sexp_of_t i)])
;;

let stdin = Unix.stdin
let stdout = Unix.stdout
let stderr = Unix.stderr

type open_flag =
Unix.open_flag =
| O_RDONLY
| O_WRONLY
| O_RDWR
| O_NONBLOCK
| O_APPEND
| O_CREAT
| O_TRUNC
| O_EXCL
| O_NOCTTY
| O_DSYNC
| O_SYNC
| O_RSYNC
| O_SHARE_DELETE
| O_CLOEXEC
#if ocaml_version >= (4, 05, 0)
| O_KEEPEXEC
#endif
[@@deriving sexp]

type file_perm = int [@@deriving of_sexp]

(* Prints out in octal, which is much more standard in Unix. *)
let sexp_of_file_perm fp = Sexp.Atom (Printf.sprintf "0o%03o" fp)

let is_rw_open_flag = function O_RDONLY | O_WRONLY | O_RDWR -> true | _ -> false

let openfile ?(perm = 0o644) ~mode filename =
  let mode_sexp () = sexp_of_list sexp_of_open_flag mode in
  if not (List.exists mode ~f:is_rw_open_flag) then
    failwithf "Unix.openfile: no read or write flag specified in mode: %s"
      (Sexp.to_string (mode_sexp ())) ()
  else
    improve (fun () -> Unix.openfile filename ~mode ~perm)
      (fun () -> [filename_r filename;
                  ("mode", mode_sexp ());
                  file_perm_r perm])
;;

let close ?restart = unary_fd ?restart Unix.close

let with_close fd ~f = protect ~f:(fun () -> f fd) ~finally:(fun () -> close fd)

let with_file ?perm file ~mode ~f = with_close (openfile file ~mode ?perm) ~f

let read_write f ?restart ?pos ?len fd ~buf =
  let pos, len =
    Ordered_collection_common.get_pos_len_exn ?pos ?len ~length:(String.length buf)
  in
  improve ?restart (fun () -> f fd ~buf ~pos ~len)
    (fun () -> [fd_r fd; ("pos", Int.sexp_of_t pos); len_r len])
;;

let read = read_write Unix.read

let write = read_write Unix.write ?restart:None

let single_write = read_write Unix.single_write

let in_channel_of_descr = Unix.in_channel_of_descr
let out_channel_of_descr = Unix.out_channel_of_descr
let descr_of_in_channel = Unix.descr_of_in_channel
let descr_of_out_channel = Unix.descr_of_out_channel

type seek_command =
Unix.seek_command =
| SEEK_SET
| SEEK_CUR
| SEEK_END
[@@deriving sexp]

type file_kind = Unix.file_kind =
| S_REG
| S_DIR
| S_CHR
| S_BLK
| S_LNK
| S_FIFO
| S_SOCK
[@@deriving sexp]

let isatty = unary_fd Unix.isatty

module Native_file = struct
  type stats =
  Unix.stats = {
    st_dev : int;
    st_ino : int;
    st_kind : file_kind;
    st_perm : file_perm;
    st_nlink : int;
    st_uid : int;
    st_gid : int;
    st_rdev : int;
    st_size : int;
    st_atime : float;
    st_mtime : float;
    st_ctime : float;
  } [@@deriving sexp]

  let stat = unary_filename Unix.stat
  let lstat = unary_filename Unix.lstat
  let fstat = unary_fd Unix.fstat

  let lseek fd pos ~mode =
    improve (fun () -> Unix.lseek fd pos ~mode)
      (fun () -> [fd_r fd;
                  ("pos", Int.sexp_of_t pos);
                  ("mode", sexp_of_seek_command mode)])
  ;;

  let truncate filename ~len =
    improve (fun () -> Unix.truncate filename ~len)
      (fun () -> [filename_r filename; len_r len])
  ;;

  let ftruncate fd ~len =
    improve (fun () -> Unix.ftruncate fd ~len)
      (fun () -> [fd_r fd; len_r len])
  ;;
end

type lock_command =
  Unix.lock_command =
  | F_ULOCK
  | F_LOCK
  | F_TLOCK
  | F_TEST
  | F_RLOCK
  | F_TRLOCK
[@@deriving sexp]

let lockf fd ~mode ~len =
  let len =
    try Int64.to_int_exn len with _ ->
      failwith "~len passed to Unix.lockf too large to fit in native int"
  in
  improve (fun () -> Unix.lockf fd ~mode ~len)
    (fun () -> [fd_r fd;
                ("mode", sexp_of_lock_command mode);
                len_r len])
;;

module Flock_command : sig
  type t

  val lock_shared : t
  val lock_exclusive : t
  val unlock : t
end = struct
  type t = int

  (* The constants are used in the [core_unix_flock] C code. *)
  let lock_shared = 0
  let lock_exclusive = 1
  let unlock = 2
end

external flock : File_descr.t -> Flock_command.t -> bool = "core_unix_flock"

let lseek fd pos ~mode =
  improve (fun () -> Unix.LargeFile.lseek fd pos ~mode)
    (fun () -> [fd_r fd;
                ("pos", Int64.sexp_of_t pos);
                ("mode", sexp_of_seek_command mode)])
;;

let len64_r len = ("len", Int64.sexp_of_t len)

let truncate filename ~len =
  improve (fun () -> Unix.LargeFile.truncate filename ~len)
    (fun () -> [filename_r filename; len64_r len])
;;

let ftruncate fd ~len =
  improve (fun () -> Unix.LargeFile.ftruncate fd ~len)
    (fun () -> [fd_r fd; len64_r len])
;;

type stats =
Unix.LargeFile.stats = {
  st_dev : int;
  st_ino : int;
  st_kind : file_kind;
  st_perm : file_perm;
  st_nlink : int;
  st_uid : int;
  st_gid : int;
  st_rdev : int;
  st_size : int64;
  st_atime : float;
  st_mtime : float;
  st_ctime : float;
} [@@deriving sexp]

let stat  = unary_filename Unix.LargeFile.stat
let lstat = unary_filename Unix.LargeFile.lstat
let fstat = unary_fd       Unix.LargeFile.fstat

let src_dst f ~src ~dst =
  improve (fun () -> f ~src ~dst)
    (fun () -> [("src", atom src); ("dst", atom dst)])
;;

let unlink = unary_filename Unix.unlink

let rename = src_dst Unix.rename

let link ?(force = false) ~target ~link_name () =
  improve
    (fun () ->
      if force then begin
        try Unix.unlink link_name
        with Unix_error (Unix.ENOENT, _, _) -> ()
      end;
      Unix.link ~src:target ~dst:link_name)
    (fun () -> [("target", atom target); ("link_name", atom link_name)])
;;

type access_permission = Unix.access_permission =
  | R_OK
  | W_OK
  | X_OK
  | F_OK
[@@deriving sexp]

let chmod filename ~perm =
  improve (fun () -> Unix.chmod filename ~perm)
    (fun () -> [filename_r filename; file_perm_r perm])
;;

let fchmod fd ~perm =
  improve (fun () -> Unix.fchmod fd ~perm)
    (fun () -> [fd_r fd; file_perm_r perm])
;;

let chown filename ~uid ~gid =
  improve (fun () -> Unix.chown filename ~uid ~gid)
    (fun () -> [filename_r filename; uid_r uid; gid_r gid])
;;

let fchown fd ~uid ~gid =
  improve (fun () -> Unix.fchown fd ~uid ~gid)
    (fun () -> [fd_r fd; uid_r uid; gid_r gid])
;;

let umask mode =
  improve (fun () -> Unix.umask mode)
    (fun () -> [("mode", atom (Printf.sprintf "0o%o" mode))])
;;

let access filename ~perm =
  improve (fun () -> Unix.access filename ~perm)
    (fun () -> [filename_r filename;
                ("perm", sexp_of_list sexp_of_access_permission perm)])
;;

let access filename perm =
  Result.try_with (fun () ->
    access filename
      ~perm:(List.map perm ~f:(function
        | `Read -> Unix.R_OK
        | `Write -> Unix.W_OK
        | `Exec -> Unix.X_OK
        | `Exists -> Unix.F_OK)))
;;

let access_exn filename perm = Result.ok_exn (access filename perm)

external remove : string -> unit = "core_unix_remove"
let remove = unary_filename remove

let%test _ =
  let dir = Core_filename.temp_dir "remove_test" "" in
  let file = dir ^/ "test" in
  Out_channel.write_all (dir ^ "/test") ~data:"testing Core.Unix.remove";
  remove file;
  remove dir;
  Result.is_error (access file [`Exists])
  && Result.is_error (access dir [`Exists])

let dup = unary_fd Unix.dup

let dup2 ~src ~dst =
  improve (fun () -> UnixNoLabels.dup2 src dst)
    (fun () -> [("src", File_descr.sexp_of_t src);
                ("dst", File_descr.sexp_of_t dst)])
;;

let%test_unit "fork_exec ~env last binding takes precedence" =
  protectx ~finally:remove (Filename.temp_file "test" "fork_exec.env.last-wins")
    ~f:(fun temp_file ->
      let env = [ "VAR", "first"; "VAR", "last" ] in
      List.iter
        [ `Replace_raw (List.map env ~f:(fun (v, s) -> v ^ "=" ^ s))
        ; `Replace env
        ; `Extend env
        ]
        ~f:(fun env ->
          waitpid_exn
            (fork_exec () ~env ~prog:"sh"
               ~argv:[ "sh"; "-c"; "echo $VAR > " ^ temp_file ]);
          [%test_result: string] ~expect:"last\n" (In_channel.read_all temp_file)))

let set_nonblock = unary_fd Unix.set_nonblock
let clear_nonblock = unary_fd Unix.clear_nonblock
let set_close_on_exec = unary_fd Unix.set_close_on_exec
let clear_close_on_exec = unary_fd Unix.clear_close_on_exec

module Open_flags = struct
  external append    : unit -> Int63.t = "unix_O_APPEND"
  external async     : unit -> Int63.t = "unix_O_ASYNC"
  external cloexec   : unit -> Int63.t = "unix_O_CLOEXEC"
  external creat     : unit -> Int63.t = "unix_O_CREAT"
  external direct    : unit -> Int63.t = "unix_O_DIRECT"
  external directory : unit -> Int63.t = "unix_O_DIRECTORY"
  external dsync     : unit -> Int63.t = "unix_O_DSYNC"
  external excl      : unit -> Int63.t = "unix_O_EXCL"
  external noatime   : unit -> Int63.t = "unix_O_NOATIME"
  external noctty    : unit -> Int63.t = "unix_O_NOCTTY"
  external nofollow  : unit -> Int63.t = "unix_O_NOFOLLOW"
  external nonblock  : unit -> Int63.t = "unix_O_NONBLOCK"
  external rdonly    : unit -> Int63.t = "unix_O_RDONLY"
  external rdwr      : unit -> Int63.t = "unix_O_RDWR"
  external rsync     : unit -> Int63.t = "unix_O_RSYNC"
  external sync      : unit -> Int63.t = "unix_O_SYNC"
  external trunc     : unit -> Int63.t = "unix_O_TRUNC"
  external wronly    : unit -> Int63.t = "unix_O_WRONLY"

  let append    = append    ()
  let async     = async     ()
  let cloexec   = cloexec   ()
  let creat     = creat     ()
  let direct    = direct    ()
  let directory = directory ()
  let dsync     = dsync     ()
  let excl      = excl      ()
  let noatime   = noatime   ()
  let noctty    = noctty    ()
  let nofollow  = nofollow  ()
  let nonblock  = nonblock  ()
  let rdonly    = rdonly    ()
  let rdwr      = rdwr      ()
  let rsync     = rsync     ()
  let sync      = sync      ()
  let trunc     = trunc     ()
  let wronly    = wronly    ()

  let known =
    [
      append,    "append";
      async,     "async";
      cloexec,   "cloexec";
      creat,     "creat";
      direct,    "direct";
      directory, "directory";
      dsync,     "dsync";
      excl,      "excl";
      noatime,   "noatime";
      noctty,    "noctty";
      nofollow,  "nofollow";
      nonblock,  "nonblock";
      rsync,     "rsync";
      sync,      "sync";
      trunc,     "trunc";

    (* We handle the access modes separately from the standard [Flags.sexp_of_t],
       because they are multibit and include the [rdonly] flag, which is zero, which
       [Flags] doesn't allow. *)

    ]
  ;;

  let access_modes =
    [ rdonly,    "rdonly";
      rdwr,      "rdwr";
      wronly,    "wronly";
    ]
  ;;

  include Flags.Make (struct
    let allow_intersecting = true
    let should_print_error = true
    let known = known
    let remove_zero_flags = true
    (* remove non existing flags, like cloexec on centos5 *)
  end)

  (* The lower two bits of the open flags are used to specify the access mode:
     rdonly, wronly, rdwr.  So, we have some code to treat those two bits together rather
     than as two separate bit flags. *)

  let access_mode t = Int63.bit_and t (Int63.of_int 3)

  let can_read t = access_mode t = rdonly || access_mode t = rdwr

  let%test _ = can_read rdonly
  let%test _ = can_read rdwr
  let%test _ = not (can_read wronly)

  let can_write t = access_mode t = wronly || access_mode t = rdwr

  let%test _ = can_write wronly
  let%test _ = can_write rdwr
  let%test _ = not (can_write rdonly)

  let sexp_of_t t =
    let a = access_mode t in
    let t, prefix =
      match List.find access_modes ~f:(fun (a', _) -> a = a') with
      | None -> t, []
      | Some (_, name) -> t - a, [Sexp.Atom name]
    in
    let rest =
      match sexp_of_t t with
      | Sexp.Atom _ as s -> [s]
      | Sexp.List l -> l
    in
    Sexp.List (prefix @ rest)
  ;;

  let check t string =
    let sexp1 = sexp_of_t t in
    let sexp2 = Sexp.of_string string in
    if Sexp.(<>) sexp1 sexp2 then
      failwiths "unequal sexps" (sexp1, sexp2) [%sexp_of: Sexp.t * Sexp.t];
  ;;

  let%test_unit _ = check rdonly            "(rdonly)"
  let%test_unit _ = check wronly            "(wronly)"
  let%test_unit _ = check rdwr              "(rdwr)"
  let%test_unit _ = check append            "(rdonly append)"
  let%test_unit _ = check (wronly + append) "(wronly append)"
end

let fcntl_getfl, fcntl_setfl =
  let module M = struct
    external unix_fcntl : Unix.file_descr -> Int63.t -> Int63.t -> Int63.t = "unix_fcntl"
    external getfl : unit -> Int63.t = "unix_F_GETFL"
    external setfl : unit -> Int63.t = "unix_F_SETFL"
    let getfl = getfl ()
    let setfl = setfl ()
  end in
  let open M in
  let fcntl_getfl fd = unix_fcntl fd getfl Int63.zero in
  let fcntl_setfl fd flags =
    let result = unix_fcntl fd setfl flags in
    (* [unix_fcntl] raised if there was an error, so if we're here, it must have returned
       zero. *)
    assert (result = Int63.zero);
  in
  fcntl_getfl, fcntl_setfl
;;

let%test_unit _ =
  let test = "unix_test_file" in
  let rm_test () = try unlink test with _ -> () in
  rm_test ();
  let fd = openfile test ~mode:[O_CREAT; O_WRONLY] in
  let flags = fcntl_getfl fd in
  assert (Open_flags.do_intersect flags Open_flags.wronly);
  assert (Open_flags.are_disjoint flags Open_flags.append);
  fcntl_setfl fd (Open_flags.(+) flags Open_flags.append);
  assert (Open_flags.do_intersect (fcntl_getfl fd) Open_flags.append);
  rm_test ()
;;

let mkdir ?(perm=0o777) dirname =
  improve (fun () -> Unix.mkdir dirname ~perm)
    (fun () -> [dirname_r dirname; file_perm_r perm])
;;

let mkdir_p ?perm dirname =
  let mkdir_if_missing ?perm dir =
    try
      mkdir ?perm dir
    with
    (* [mkdir] on MacOSX returns [EISDIR] instead of [EEXIST] if the directory already
       exists. *)
    | Unix_error ((EEXIST | EISDIR), _, _) -> ()
    | e -> raise e
  in
  let init,dirs =
    match Core_filename.parts dirname with
    | [] -> assert false
    | init :: dirs -> (init, dirs)
  in
  mkdir_if_missing ?perm init;
  let (_:string) = (* just using the fold for the side effects and accumulator *)
    (* This must be [fold_left], not [fold_right]. *)
    List.fold_left dirs ~init ~f:(fun acc dir ->
      let dir = Filename.concat acc dir in
      mkdir_if_missing ?perm dir;
      dir)
  in
  ()
;;

let rmdir = unary_dirname Unix.rmdir
let chdir = unary_dirname Unix.chdir
let getcwd = Unix.getcwd
let chroot = unary_dirname Unix.chroot

type dir_handle = Unix.dir_handle

let opendir ?restart = unary_dirname ?restart Unix.opendir
let readdir = unary_dir_handle Unix.readdir (* Non-intr *)
let readdir_opt dh =
  match readdir dh with
  | entry                 -> Some entry
  | exception End_of_file -> None
let rewinddir = unary_dir_handle Unix.rewinddir (* Non-intr *)
(* if closedir is passed an already closed file handle it will try to call
  dirfd on it to get a file descriptor for the error message, which will fail
  with invalid argument because closedir sets the fd to null *)
let closedir = (* Non-intr *)
  unary_dir_handle (fun dh ->
    try Unix.closedir dh with | Invalid_argument _ -> ())

let pipe () = UnixNoLabels.pipe ()

let mkfifo name ~perm =
  improve (fun () -> Unix.mkfifo name ~perm)
    (fun () -> [("name", atom name); file_perm_r perm])
;;

module Process_info = struct
  (* Any change to the order of these fields must be accompanied by a
     corresponding change to unix_stubs.c:ml_create_process. *)
  type t =
    { pid : Pid.t;
      stdin : File_descr.t;
      stdout : File_descr.t;
      stderr : File_descr.t;
    }
  [@@deriving sexp]
end

let create_process_backend = ref (
  match Core_sys.getenv "CORE_CREATE_PROCESS_DONT_USE_SPAWN_BACKEND_UNLESS_TOLD_TO" with
  | None ->
    `spawn_vfork
  | Some _ ->
    match Core_sys.getenv "CORE_CREATE_PROCESS_USE_SPAWN_BACKEND" with
    | None -> `ml_create_process
    | Some _ ->
      `spawn_vfork)
;;

external create_process_old
  :  ?working_dir : string
  -> prog        : string
  -> args : string array
  -> env : string array
  -> search_path : bool
  -> Process_info.t
  = "ml_create_process"
;;

let create_process_internal
  :  working_dir : string option
  -> prog        : string
  -> argv        : string list
  -> env         : string list
  -> Process_info.t
  =
  fun ~working_dir ~prog ~argv ~env ->
    let close_on_err = ref [] in
    let safe_pipe () =
      let (fd_read, fd_write) as result = Spawn.safe_pipe () in
      close_on_err := fd_read :: fd_write :: !close_on_err;
      result
    in
    try
      let in_read,  in_write  = safe_pipe () in
      let out_read, out_write = safe_pipe () in
      let err_read, err_write = safe_pipe () in
      let pid =
        Spawn.spawn
          ?cwd:(Option.map working_dir ~f:(fun x -> Spawn.Working_dir.Path x))
          ~prog
          ~argv
          ~env
          ~stdin:in_read
          ~stdout:out_write
          ~stderr:err_write
          ()
        |> Pid.of_int
      in
      close in_read; close out_write; close err_write;
      { pid; stdin = in_write; stdout = out_read; stderr = err_read; }
    with exn ->
      List.iter !close_on_err ~f:(fun x -> try close x with _ -> ());
      raise exn
;;

module Execvp_emulation : sig
  (* This is a reimplementation of execvp semantics with two main differences:
     - it does [spawn] instead of [execve] and returns its result on success
     - it checks file existence and access rights before trying to spawn.
     This optimization is valuable because a failed [spawn] is much more expensive than a
     failed [execve]. *)
  val run
    :  working_dir : string option
    -> spawn       : (prog:string -> argv:string list -> 'a)
    -> env_lookup  : (string -> string option)
    -> prog        : string
    -> args        : string list
    -> 'a

end = struct

  let get_path env_lookup =
    env_lookup "PATH"
    |> Option.value_map ~f:(String.split ~on:':') ~default:[""; "/bin"; "/usr/bin"]
    |> List.map ~f:(function
      | "" -> "."
      | x  -> x)
  ;;

  let candidate_paths ~env_lookup prog =
    (* [assert] is to make bugs less subtle if we try to make this
       portable to non-POSIX in the future. *)
    assert (Filename.dir_sep = "/");
    if String.contains prog '/' then
      [ prog ]
    else
      List.map (get_path env_lookup) ~f:(fun h -> h ^/ prog)
  ;;

  type 'a spawn1_result =
    | Eaccess           of exn
    | Enoent_or_similar of exn
    | Ok                of 'a

  let run ~working_dir ~spawn ~env_lookup ~prog ~args =
    let argv = prog :: args in
    let spawn1 candidate =
      match
        (try
           Unix.access
             (if not (Filename.is_relative candidate) then
                candidate
              else
                match working_dir with
                | Some working_dir -> working_dir ^/ candidate
                | None -> candidate)
             ~perm:[Unix.X_OK]
         with Unix_error (code, _, args) ->
           raise (Unix_error (code, "Core.Unix.create_process", args)));
        spawn ~prog:candidate ~argv
      with
      | exception Unix_error (ENOEXEC, _, _) -> Ok (
        (* As crazy as it looks, this is what execvp does. It's even documented in the man
           page. *)
        spawn
          ~prog:"/bin/sh"
          ~argv:("/bin/sh" :: candidate :: args))
      | exception (Unix_error (EACCES, _, _) as exn) ->
        Eaccess exn
      | exception (Unix_error (
        (ENOENT | (* ESTALE | *) ENOTDIR | ENODEV | ETIMEDOUT), _, _) as exn) ->
        Enoent_or_similar exn
      | pid -> Ok pid
    in
    let rec go first_eaccess = function
      | [] -> assert false (* [candidate_paths] can't return an empty list *)
      | [ candidate ] ->
        (match spawn1 candidate with
         | Eaccess exn
         | Enoent_or_similar exn ->
           raise (Option.value first_eaccess ~default:exn)
         | Ok pid -> pid)
      | candidate :: (_ :: _ as candidates) ->
        match spawn1 candidate with
        | Eaccess exn ->
          let first_eaccess = Some (Option.value first_eaccess ~default:exn) in
          go first_eaccess candidates
        | Enoent_or_similar _exn ->
          go first_eaccess candidates
        | Ok pid -> pid
    in
    go None (candidate_paths ~env_lookup prog)
  ;;
end

let create_process_env ?working_dir ~prog ~args ~env () =
  let env_assignments = env_assignments env in
  match !create_process_backend with
  | `spawn_vfork ->
    Execvp_emulation.run
      ~prog
      ~args
      ~env_lookup:(env_lookup env)
      ~working_dir
      ~spawn:(fun ~prog ~argv ->
        create_process_internal
          ~working_dir
          ~prog
          ~argv
          ~env:env_assignments)
  | `ml_create_process ->
    create_process_old
      ?working_dir
      ~search_path:true
      ~prog
      ~args:(Array.of_list args)
      ~env:(Array.of_list env_assignments)

let create_process_env ?working_dir ~prog ~args ~env () =
  improve (fun () -> create_process_env ?working_dir ~prog ~args ~env ())
    (fun () ->
      [("prog", atom prog);
       ("args", sexp_of_list atom args);
       ("env", sexp_of_env env)])

let create_process ~prog ~args =
  improve (fun () -> create_process_env ~prog ~args ~env:(`Extend []) ())
    (fun () ->
      [("prog", atom prog);
       ("args", sexp_of_list atom args)])

let make_open_process f command =
  improve (fun () -> f command)
    (fun () -> [("command", atom command)])

let open_process_in = make_open_process Unix.open_process_in
let open_process_out = make_open_process Unix.open_process_out
let open_process = make_open_process Unix.open_process

module Process_channels = struct
  type t = {
    stdin : Out_channel.t;
    stdout : In_channel.t;
    stderr : In_channel.t;
  }
end

let open_process_full command ~env =
  improve (fun () ->
    let stdout, stdin, stderr = Unix.open_process_full command ~env in
    { Process_channels.stdin = stdin; stdout = stdout; stderr = stderr })
    (fun () -> [("command", atom command);
                ("env", sexp_of_array atom env)])
;;

let close_process_in ic = Exit_or_signal.of_unix (Unix.close_process_in ic)
let close_process_out oc = Exit_or_signal.of_unix (Unix.close_process_out oc)

let close_process (ic, oc) = Exit_or_signal.of_unix (Unix.close_process (ic, oc))

let close_process_full c =
  let module C = Process_channels in
  Exit_or_signal.of_unix (Unix.close_process_full (c.C.stdout, c.C.stdin, c.C.stderr))
;;

let symlink = src_dst (fun ~src ~dst -> Unix.symlink ?to_dir:None ~src ~dst)
let readlink = unary_filename Unix.readlink

module Select_fds = struct
  type t =
    { read : File_descr.t list;
      write : File_descr.t list;
      except : File_descr.t list;
    }
  [@@deriving sexp_of]

  let empty = { read = []; write = []; except = [] }
end

type select_timeout = [ `Never | `Immediately | `After of Time_ns.Span.t ]
[@@deriving sexp_of]

let select ?restart ~read ~write ~except ~timeout () =
  improve ?restart (fun () ->
    let timeout =
      match timeout with
      | `Never -> -1.
      | `Immediately -> 0.
      | `After span ->
        if Time_ns.Span.( < ) span Time_ns.Span.zero
        then 0.
        else Time_ns.Span.to_sec span
    in
    let read, write, except = Unix.select ~read ~write ~except ~timeout in
    { Select_fds. read; write; except })
    (fun () ->
       [("read", sexp_of_list File_descr.sexp_of_t read);
        ("write", sexp_of_list File_descr.sexp_of_t write);
        ("except", sexp_of_list File_descr.sexp_of_t except);
        ("timeout", [%sexp_of: select_timeout] timeout)])
;;

let pause = Unix.pause

type process_times =
  Unix.process_times = {
  tms_utime  : float;
  tms_stime  : float;
  tms_cutime : float;
  tms_cstime : float;
}
[@@deriving sexp]


type tm =
    Unix.tm = {
    (* DON'T CHANGE THIS RECORD WITHOUT UPDATING unix_time_stubs.c!!!

       The compiler will notice if the runtime's Unix.tm changes, and we must then update
       unix_time_stubs.c, not just this copy of the definition. *)
    tm_sec   : int;
    tm_min   : int;
    tm_hour  : int;
    tm_mday  : int;
    tm_mon   : int;
    tm_year  : int;
    tm_wday  : int;
    tm_yday  : int;
    tm_isdst : bool;
  } [@@deriving sexp]

let time = Unix.time
let gettimeofday = Unix.gettimeofday

external strftime : Unix.tm -> string -> string = "core_time_ns_strftime"
external localtime : float -> Unix.tm = "core_localtime"
external gmtime    : float -> Unix.tm = "core_gmtime"
external timegm    : Unix.tm -> float = "core_timegm" (* the inverse of gmtime *)

let mktime = Unix.mktime
let alarm  = Unix.alarm
let sleep  = Unix.sleep
let times  = Unix.times
let utimes = Unix.utimes

external strptime : fmt:string -> string -> Unix.tm = "unix_strptime"

let%test_unit "record format hasn't changed" =
  (* Exclude the time zone (%Z) because it depends on the location. *)
  [%test_result: string] ~expect:"1907-07-05 04:03:08; wday=2; yday=010"
    (strftime
       { tm_sec = 8; tm_min = 3; tm_hour = 4; tm_mday = 5; tm_mon = 6; tm_year = 7;
         tm_wday = 2; tm_yday = 9; tm_isdst = true }
       "%F %T; wday=%u; yday=%j")

let%test _ =
  let res = strptime ~fmt:"%Y-%m-%d %H:%M:%S" "2012-05-23 10:14:23" in
  let res =
    (* fill in optional fields if they are missing *)
    let tm_wday = if res.Unix.tm_wday = 0 then 3   else res.Unix.tm_wday in
    let tm_yday = if res.Unix.tm_yday = 0 then 143 else res.Unix.tm_yday in
    { res with Unix. tm_wday; tm_yday }
  in
  res = {Unix.
    tm_sec   = 23;
    tm_min   = 14;
    tm_hour  = 10;
    tm_mday  = 23;
    tm_mon   = 4;
    tm_year  = 2012 - 1900;
    tm_wday  = 3;
    tm_yday  = 143;
    tm_isdst = false; }

let%test _ =
  try
    ignore (strptime ~fmt:"%Y-%m-%d" "2012-05-");
    false
  with
  | _ -> true

type interval_timer = Unix.interval_timer =
  | ITIMER_REAL
  | ITIMER_VIRTUAL
  | ITIMER_PROF
[@@deriving sexp]

type interval_timer_status = Unix.interval_timer_status = {
  it_interval : float;
  it_value : float;
}
[@@deriving sexp]

let getitimer = Unix.getitimer
let setitimer = Unix.setitimer

let getuid = Unix.getuid
let geteuid = Unix.geteuid

let setuid uid =
  improve (fun () -> Unix.setuid uid)
    (fun () -> [("uid", Int.sexp_of_t uid)])

let getgid = Unix.getgid
let getegid = Unix.getegid

let setgid gid =
  improve (fun () -> Unix.setgid gid)
    (fun () -> [("gid", Int.sexp_of_t gid)])

let getgroups = Unix.getgroups

let make_by f make_exn =
  let normal arg = try Some (f arg) with Not_found -> None in
  let exn arg = try f arg with Not_found -> raise (make_exn arg) in
  (normal, exn)
;;

module Passwd = struct
  type t =
    { name : string;
      passwd : string;
      uid : int;
      gid : int;
      gecos : string;
      dir : string;
      shell : string;
    }
  [@@deriving compare, sexp]

  let of_unix u =
    let module U = Unix in
    { name = u.U.pw_name;
      passwd = u.U.pw_passwd;
      uid = u.U.pw_uid;
      gid = u.U.pw_gid;
      gecos = u.U.pw_gecos;
      dir = u.U.pw_dir;
      shell = u.U.pw_shell;
    }
  ;;

  exception Getbyname of string [@@deriving sexp]

  let (getbyname, getbyname_exn) =
    make_by
      (fun name -> of_unix (Unix.getpwnam name))
      (fun s -> Getbyname s)
  ;;

  exception Getbyuid of int [@@deriving sexp]

  let (getbyuid, getbyuid_exn) =
    make_by
      (fun uid -> of_unix (Unix.getpwuid uid))
      (fun s -> Getbyuid s)
  ;;

  module Low_level = struct
    external core_setpwent : unit -> unit = "core_setpwent" ;;
    external core_endpwent : unit -> unit = "core_endpwent" ;;
    external core_getpwent : unit -> Unix.passwd_entry = "core_getpwent" ;;
    let setpwent = core_setpwent ;;

    let getpwent_exn () = of_unix (core_getpwent ()) ;;
    let getpwent () = Option.try_with (fun () -> getpwent_exn ()) ;;
    let endpwent = core_endpwent ;;
  end ;;

  let pwdb_lock = Mutex0.create () ;;

  let getpwents () =
    Mutex0.critical_section pwdb_lock ~f:(fun () ->
      begin
        Low_level.setpwent ();
        Exn.protect
          ~f:(fun () ->
            let rec loop acc =
              try
                let ent = Low_level.getpwent_exn () in
                loop (ent :: acc)
              with
              | End_of_file -> List.rev acc
            in
            loop [])
          ~finally:(fun () -> Low_level.endpwent ())
      end)
  ;;
end

module Group = struct
  type t =
    { name : string;
      passwd : string;
      gid : int;
      mem : string array;
    }
  [@@deriving sexp_of]

  let of_unix u =
    { name = u.Unix.gr_name;
      passwd = u.Unix.gr_passwd;
      gid = u.Unix.gr_gid;
      mem = u.Unix.gr_mem;
    }
  ;;

  exception Getbyname of string [@@deriving sexp]

  let (getbyname, getbyname_exn) =
    make_by (fun name -> of_unix (Unix.getgrnam name)) (fun s -> Getbyname s)
  ;;

  exception Getbygid of int [@@deriving sexp]

  let (getbygid, getbygid_exn) =
    make_by (fun gid -> of_unix (Unix.getgrgid gid)) (fun s -> Getbygid s)
  ;;
end

(* The standard getlogin function goes through utmp which is unreliable,
   see the BUGS section of getlogin(3) *)
let _getlogin_orig = Unix.getlogin
let getlogin () = (Unix.getpwuid (getuid ())).Unix.pw_name

module Protocol_family = struct
  type t = [ `Unix | `Inet | `Inet6 ]
  [@@deriving bin_io, sexp]

  let of_unix = function
    | Unix.PF_UNIX -> `Unix
    | Unix.PF_INET -> `Inet
    | Unix.PF_INET6 -> `Inet6
  ;;
end

let gethostname = Unix.gethostname

module Inet_addr0 = struct
  module Stable = struct
    module V1 = struct
      module T0 = struct
        type t = Unix.inet_addr

        let of_string = Unix.inet_addr_of_string
        let to_string = Unix.string_of_inet_addr

        (* Unix.inet_addr is represented as either a "struct in_addr" or a "struct
           in6_addr" stuffed into an O'Caml string, so polymorphic compare will work. *)
        let compare = Pervasives.compare
        let hash_fold_t hash t = hash_fold_int hash (Hashtbl.hash t)
        let hash = Ppx_hash_lib.Std.Hash.of_fold hash_fold_t
      end
      module T1 = struct
        include T0
        include Sexpable.Of_stringable (T0)
        include Binable.Of_stringable  (T0)
      end
      include T1
      include Comparable.Make(T1)
    end
  end
  include Stable.V1
  include Stable_unit_test.Make (struct
      type nonrec t = t [@@deriving sexp, bin_io]
      let equal = equal
      ;;

      let tests =
        (* IPv4 *)
        [ of_string "0.0.0.0"        , "0.0.0.0"        , "\0070.0.0.0"
        ; of_string "10.0.0.0"       , "10.0.0.0"       , "\00810.0.0.0"
        ; of_string "127.0.0.1"      , "127.0.0.1"      , "\009127.0.0.1"
        ; of_string "192.168.1.101"  , "192.168.1.101"  , "\013192.168.1.101"
        ; of_string "255.255.255.255", "255.255.255.255", "\015255.255.255.255"
        (* IPv6 *)
        ; of_string "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
          "2001:db8:85a3::8a2e:370:7334",
          "\0282001:db8:85a3::8a2e:370:7334"
        ; of_string "2001:db8:85a3:0:0:8a2e:370:7334",
          "2001:db8:85a3::8a2e:370:7334",
          "\0282001:db8:85a3::8a2e:370:7334"
        ; of_string "2001:db8:85a3::8a2e:370:7334",
          "2001:db8:85a3::8a2e:370:7334",
          "\0282001:db8:85a3::8a2e:370:7334"
        ; of_string "0:0:0:0:0:0:0:1", "::1", "\003::1"
        ; of_string "::1"            , "::1", "\003::1"
        ; of_string "0:0:0:0:0:0:0:0", "::", "\002::"
        ; of_string "::"             , "::", "\002::"
        ; of_string "::ffff:c000:0280"  , "::ffff:192.0.2.128", "\018::ffff:192.0.2.128"
        ; of_string "::ffff:192.0.2.128", "::ffff:192.0.2.128", "\018::ffff:192.0.2.128"
        ; of_string "2001:0db8::0001", "2001:db8::1"  , "\0112001:db8::1"
        ; of_string "2001:db8::1"    , "2001:db8::1"  , "\0112001:db8::1"
        ; of_string "2001:db8::2:1"  , "2001:db8::2:1", "\0132001:db8::2:1"
        ; of_string "2001:db8:0000:1:1:1:1:1",
          "2001:db8:0:1:1:1:1:1",
          "\0202001:db8:0:1:1:1:1:1"
        ; of_string "2001:db8::1:1:1:1:1",
          "2001:db8:0:1:1:1:1:1",
          "\0202001:db8:0:1:1:1:1:1"
        ; of_string "2001:db8:0:1:1:1:1:1",
          "2001:db8:0:1:1:1:1:1",
          "\0202001:db8:0:1:1:1:1:1"
        ; of_string "2001:db8:0:0:1:0:0:1", "2001:db8::1:0:0:1", "\0172001:db8::1:0:0:1"
        ; of_string "2001:db8:0:0:1::1"   , "2001:db8::1:0:0:1", "\0172001:db8::1:0:0:1"
        ; of_string "2001:db8::1:0:0:1"   , "2001:db8::1:0:0:1", "\0172001:db8::1:0:0:1"
        ; of_string "2001:DB8::1", "2001:db8::1", "\0112001:db8::1"
        ; of_string "2001:db8::1", "2001:db8::1", "\0112001:db8::1"
        ]
      ;;
    end)
end

module Host = struct
  type t =
    { name : string;
      aliases : string array;
      family : Protocol_family.t;
      addresses : Inet_addr0.t array;
    }
  [@@deriving sexp_of]

  let of_unix u =
    { name = u.Unix.h_name;
      aliases = u.Unix.h_aliases;
      family = Protocol_family.of_unix u.Unix.h_addrtype;
      addresses = u.Unix.h_addr_list;
    }
  ;;

  exception Getbyname of string [@@deriving sexp]

  let (getbyname, getbyname_exn) =
    make_by (fun name -> of_unix (Unix.gethostbyname name)) (fun s -> Getbyname s)
  ;;

  exception Getbyaddr of Inet_addr0.t [@@deriving sexp]

  let (getbyaddr, getbyaddr_exn) =
    make_by (fun addr -> of_unix (Unix.gethostbyaddr addr)) (fun a -> Getbyaddr a)
  ;;

  let have_address_in_common h1 h2 =
    let addrs1 = Inet_addr0.Set.of_array h1.addresses in
    let addrs2 = Inet_addr0.Set.of_array h2.addresses in
    not (Inet_addr0.Set.is_empty (Inet_addr0.Set.inter addrs1 addrs2))
  ;;
end

module Inet_addr = struct
  include Inet_addr0

  exception Get_inet_addr of string * string [@@deriving sexp]

  let of_string_or_getbyname name =
    try of_string name
    with Failure _ ->
      match Host.getbyname name with
      | None -> raise (Get_inet_addr (name, "host not found"))
      | Some host ->
        match host.Host.family with
        | `Unix -> assert false  (* impossible *)
        | `Inet | `Inet6 ->
          let addrs = host.Host.addresses in
          if Int.(>) (Array.length addrs) 0 then addrs.(0)
          else raise (Get_inet_addr (name, "empty addrs"))
  ;;

  module Blocking_sexp = struct
    module T = struct
      include Inet_addr0
      let of_string = of_string_or_getbyname
    end
    include T
    include Sexpable.Of_stringable (T)
  end
  ;;

  let t_of_sexp = Blocking_sexp.t_of_sexp

  let bind_any       = Unix.inet_addr_any
  let bind_any_inet6 = Unix.inet6_addr_any
  let localhost       = Unix.inet_addr_loopback
  let localhost_inet6 = Unix.inet6_addr_loopback

  let int32_to_ipv4_string =
    let open Int.O in
    let octet_digits octet =
      if octet >= 100 then 3 else
      if octet >=  10 then 2 else
        1
    in
    let char_of_digit digit =
      Char.of_int_exn (Char.to_int '0' + digit)
    in
    let write_char string ~pos char =
      string.[pos] <- char;
      Int.succ pos
    in
    let write_digit string ~pos digit =
      write_char string ~pos (char_of_digit digit)
    in
    let write_octet string ~pos octet =
      let pos =
        if octet >= 100
        then write_digit string ~pos (octet / 100)
        else pos
      in
      let pos =
        if octet >= 10
        then write_digit string ~pos ((octet / 10) % 10)
        else pos
      in
      write_digit string ~pos (octet % 10)
    in
    fun int32 ->
      let open Int32.O in
      let octet1 = (int32 lsr 24) land 0xFFl |> Int32.to_int_exn in
      let octet2 = (int32 lsr 16) land 0xFFl |> Int32.to_int_exn in
      let octet3 = (int32 lsr  8) land 0xFFl |> Int32.to_int_exn in
      let octet4 = (int32 lsr  0) land 0xFFl |> Int32.to_int_exn in
      let open Int.O in
      let len =
        octet_digits octet1 +
        octet_digits octet2 +
        octet_digits octet3 +
        octet_digits octet4 +
        3
      in
      let string = String.create len in
      let pos = 0 in
      let pos = write_octet string ~pos octet1 in
      let pos = write_char  string ~pos '.'    in
      let pos = write_octet string ~pos octet2 in
      let pos = write_char  string ~pos '.'    in
      let pos = write_octet string ~pos octet3 in
      let pos = write_char  string ~pos '.'    in
      let pos = write_octet string ~pos octet4 in
      assert (Int.equal pos len);
      string

  let int32_of_ipv4_string =
    let open Int.O in
    let [@inline never] invalid string =
      raise_s [%message "not a valid IPv4 address" string]
    in
    let rec find_sep string ~len ~pos =
      if pos < len && not (Char.equal '.' (String.unsafe_get string pos))
      then find_sep string ~len ~pos:(pos + 1)
      else pos
    in
    let find_sep_exn string ~len ~pos =
      let pos = find_sep string ~len ~pos in
      if pos >= len then invalid string;
      pos
    in
    let find_end_exn string ~len ~pos =
      let pos = find_sep string ~len ~pos in
      if pos < len then invalid string
    in
    let read_digit string pos =
      match String.unsafe_get string pos with
      | '0'..'9' as char -> Char.to_int char - Char.to_int '0'
      | _                -> invalid string
    in
    let rec read_int string start until ~prefix =
      if start = until
      then prefix
      else begin
        read_int string (start + 1) until
          ~prefix:((prefix * 10) + (read_digit string start))
      end
    in
    let read_octet string start until =
      let len = until - start in
      if len < 1 || len > 3 then invalid string;
      let int = read_int string start until ~prefix:0 in
      if int < 0 || int > 255 then invalid string;
      int
    in
    let int32_of_octets octet1 octet2 octet3 octet4 =
      let open Int32.O in
      ((Caml.Int32.of_int octet1) lsl 24) lor
      ((Caml.Int32.of_int octet2) lsl 16) lor
      ((Caml.Int32.of_int octet3) lsl  8) lor
      ((Caml.Int32.of_int octet4) lsl  0)
    in
    fun string ->
      let len = String.length string in
      let sep1 = find_sep_exn string ~len ~pos:0          in
      let sep2 = find_sep_exn string ~len ~pos:(sep1 + 1) in
      let sep3 = find_sep_exn string ~len ~pos:(sep2 + 1) in
      let ()   = find_end_exn string ~len ~pos:(sep3 + 1) in
      let octet1 = read_octet string 0          sep1 in
      let octet2 = read_octet string (sep1 + 1) sep2 in
      let octet3 = read_octet string (sep2 + 1) sep3 in
      let octet4 = read_octet string (sep3 + 1) len  in
      int32_of_octets octet1 octet2 octet3 octet4

  let inet4_addr_of_int32 int32 =
    of_string (int32_to_ipv4_string int32)

  let inet4_addr_to_int32_exn addr =
    int32_of_ipv4_string (to_string addr)

  (* Can we convert ip addr to an int? *)
  let test_inet4_addr_to_int32 str num =
    let inet = of_string str in
    [%test_result: Int32.t] (inet4_addr_to_int32_exn inet) ~expect:num

  let%test_unit _ = test_inet4_addr_to_int32 "0.0.0.1"                  1l
  let%test_unit _ = test_inet4_addr_to_int32 "1.0.0.0"          0x1000000l
  let%test_unit _ = test_inet4_addr_to_int32 "255.255.255.255" 0xffffffffl
  let%test_unit _ = test_inet4_addr_to_int32 "172.25.42.1"     0xac192a01l
  let%test_unit _ = test_inet4_addr_to_int32 "4.2.2.1"          0x4020201l
  let%test_unit _ = test_inet4_addr_to_int32 "8.8.8.8"          0x8080808l
  let%test_unit _ = test_inet4_addr_to_int32 "173.194.73.103"  0xadc24967l
  let%test_unit _ = test_inet4_addr_to_int32 "98.139.183.24"   0x628bb718l
  let%test_unit _ = test_inet4_addr_to_int32 "0.0.0.0"                  0l
  let%test_unit _ = test_inet4_addr_to_int32 "127.0.0.1"       0x7F000001l
  let%test_unit _ = test_inet4_addr_to_int32 "239.0.0.0"       0xEF000000l
  let%test_unit _ = test_inet4_addr_to_int32 "255.255.255.255" 0xFFFFFFFFl

  (* And from an int to a string? *)
  let test_inet4_addr_of_int32 num str =
    let inet = of_string str in
    [%test_result: t] (inet4_addr_of_int32 num) ~expect:inet

  let%test_unit _ = test_inet4_addr_of_int32 0xffffffffl "255.255.255.255"
  let%test_unit _ = test_inet4_addr_of_int32          0l "0.0.0.0"
  let%test_unit _ = test_inet4_addr_of_int32 0x628bb718l "98.139.183.24"
  let%test_unit _ = test_inet4_addr_of_int32 0xadc24967l "173.194.73.103"

  (* And round trip for kicks *)
  let%test_unit _ =
    let inet  = of_string "4.2.2.1" in
    let inet' = inet4_addr_of_int32 (inet4_addr_to_int32_exn inet) in
    if inet <> inet' then
      failwithf "round-tripping %s produced %s"
        (to_string inet) (to_string inet') ()
end

(** IPv6 addresses are not supported.
    The RFC regarding how to properly format an IPv6 string is...painful.

    Note the 0010 and 0000:
    # "2a03:2880:0010:1f03:face:b00c:0000:0025" |> Unix.Inet_addr.of_string |!
      Unix.Inet_addr.to_string ;;
      - : string = "2a03:2880:10:1f03:face:b00c:0:25"
*)

module Cidr = struct
  module Stable = struct
    module V1 = struct
      module T0 = struct
        (* [address] is always normalized such that the (32 - [bits]) least-significant
           bits are zero. *)
        type t =
          { address : int32; (* IPv4 only *)
            bits    : int;
          }
        [@@deriving fields, bin_io, compare, hash, compare]

        let normalized_address ~base ~bits =
          if bits = 0
          then 0l
          else
            let shift = 32 - bits in
            Int32.(shift_left (shift_right_logical base shift) shift)

        let create ~base_address ~bits =
          if bits < 0 || bits > 32 then
            failwithf "%d is an invalid number of mask bits (0 <= bits <= 32)" bits ();
          let base = Inet_addr.inet4_addr_to_int32_exn base_address in
          let address = normalized_address ~base ~bits in
          { address; bits }

        let of_string s =
          match String.split ~on:'/' s with
          | [s_inet_address ; s_bits] ->
            create
              ~base_address:(Inet_addr.of_string s_inet_address)
              ~bits:(Int.of_string s_bits)
          | _ -> failwithf "Couldn't parse '%s' into a CIDR address/bits pair" s ()

        let to_string t =
          let addr = Inet_addr.inet4_addr_of_int32 t.address in
          sprintf "%s/%d" (Inet_addr.to_string addr) t.bits
      end
      module T1 = Sexpable.Stable.Of_stringable.V1(T0)
      module T2 = Comparator.Stable.V1.Make(struct include T0 include T1 end)
      module T3 = Comparable.Stable.V1.Make(struct include T0 include T1 include T2 end)
      include T0
      include T1
      include T2
      include T3
    end
  end

  include Stable.V1.T0
  include Stable.V1.T1
  include Stable.V1.T2

  let invariant t =
    assert (t.bits >= 0 && t.bits <= 32);
    assert (Int32.equal t.address (normalized_address ~base:t.address ~bits:t.bits))

  let base_address t =
    Inet_addr.inet4_addr_of_int32 t.address

  let netmask_of_bits t =
    Int32.shift_left 0xffffffffl (32 - t.bits) |> Inet_addr.inet4_addr_of_int32

  let does_match_int32 t address =
    Int32.equal t.address (normalized_address ~base:address ~bits:t.bits)

  let does_match t inet_addr =
    match Inet_addr.inet4_addr_to_int32_exn inet_addr with
    | exception _ -> false (* maybe they tried to use IPv6 *)
    | address     -> does_match_int32 t address

  let multicast = of_string "224.0.0.0/4"

  let is_subset t ~of_ =
    bits of_ <= bits t
    && does_match_int32 of_ t.address

  let all_matching_addresses t =
    Sequence.unfold ~init:t.address ~f:(fun address ->
      if does_match_int32 t address
      then Some (Inet_addr.inet4_addr_of_int32 address, Int32.succ address)
      else None)

  include Identifiable.Make_using_comparator(struct
    let module_name = "Core.Unix.Cidr"
    include Stable.V1.T0
    include Stable.V1.T1
    include Stable.V1.T2
  end)
end

module Protocol = struct
  type t =
    { name : string;
      aliases : string array;
      proto : int;
    }
  [@@deriving sexp]

  let of_unix u =
    { name = u.Unix.p_name;
      aliases = u.Unix.p_aliases;
      proto = u.Unix.p_proto;
    }

  exception Getbyname of string [@@deriving sexp]
  let (getbyname, getbyname_exn) =
    make_by (fun name -> of_unix (Unix.getprotobyname name))
      (fun s -> Getbyname s)
  ;;

  exception Getbynumber of int [@@deriving sexp]
  let (getbynumber, getbynumber_exn) =
    make_by (fun i -> of_unix (Unix.getprotobynumber i))
      (fun i -> Getbynumber i)
  ;;
end

module Service = struct
  type t =
    { name : string;
      aliases : string array;
      port : int;
      proto : string;
    }
  [@@deriving sexp]

  let of_unix u =
    { name = u.Unix.s_name;
      aliases = u.Unix.s_aliases;
      port = u.Unix.s_port;
      proto = u.Unix.s_proto;
    }

  exception Getbyname of string * string [@@deriving sexp]

  let getbyname_exn name ~protocol =
    try of_unix (Unix.getservbyname name ~protocol)
    with Not_found -> raise (Getbyname (name, protocol))
  ;;

  let getbyname name ~protocol =
    try Some (of_unix (Unix.getservbyname name ~protocol))
    with _ -> None
  ;;

  exception Getbyport of int * string [@@deriving sexp]

  let getbyport_exn num ~protocol =
    try of_unix (Unix.getservbyport num ~protocol)
    with Not_found -> raise (Getbyport (num, protocol))
  ;;

  let getbyport num ~protocol =
    try Some (of_unix (Unix.getservbyport num ~protocol))
    with Not_found -> None
  ;;
end

type socket_domain = Unix.socket_domain =
  | PF_UNIX
  | PF_INET
  | PF_INET6
[@@deriving sexp, bin_io]

type socket_type = Unix.socket_type =
  | SOCK_STREAM
  | SOCK_DGRAM
  | SOCK_RAW
  | SOCK_SEQPACKET
[@@deriving sexp, bin_io]

type sockaddr = Unix.sockaddr =
  | ADDR_UNIX of string
  | ADDR_INET of Inet_addr.t * int
[@@deriving sexp_of, bin_io]

type sockaddr_blocking_sexp = Unix.sockaddr =
  | ADDR_UNIX of string
  | ADDR_INET of Inet_addr.Blocking_sexp.t * int
[@@deriving sexp, bin_io]

let sockaddr_of_sexp = sockaddr_blocking_sexp_of_sexp

let domain_of_sockaddr = Unix.domain_of_sockaddr

let addr_r addr = ("addr", sexp_of_sockaddr addr)

let socket_or_pair f ~domain ~kind ~protocol =
  improve (fun () -> f domain kind protocol)
    (fun () -> [("domain", sexp_of_socket_domain domain);
                ("kind", sexp_of_socket_type kind);
                ("protocol", Int.sexp_of_t protocol)])
;;

let socket =
  socket_or_pair
    (fun domain kind protocol -> UnixNoLabels.socket domain kind protocol)
let socketpair =
  socket_or_pair
    (fun domain kind protocol -> UnixNoLabels.socketpair domain kind protocol)

let accept fd =
  let fd, addr = unary_fd Unix.accept fd in
  let addr =
    match addr with
    | ADDR_UNIX _ -> ADDR_UNIX ""
    | ADDR_INET _ -> addr
  in
  fd, addr

let bind fd ~addr =
  improve (fun () -> Unix.bind fd ~addr)
    (fun () -> [fd_r fd; addr_r addr])
;;

let connect fd ~addr =
  improve (fun () -> Unix.connect fd ~addr)
    (fun () -> [fd_r fd; addr_r addr])
;;

let listen fd ~backlog =
  improve (fun () -> Unix.listen fd ~max:backlog)
    (fun () -> [fd_r fd; ("backlog", Int.sexp_of_t backlog)])
;;

type shutdown_command = Unix.shutdown_command =
  | SHUTDOWN_RECEIVE
  | SHUTDOWN_SEND
  | SHUTDOWN_ALL
[@@deriving sexp]

let shutdown fd ~mode =
  improve (fun () ->
    try
      Unix.shutdown fd ~mode
    with
    (* the error below is benign, it means that the other side disconnected *)
    | Unix.Unix_error (Unix.ENOTCONN, _, _) -> ())
    (fun () -> [fd_r fd; ("mode", sexp_of_shutdown_command mode)])
;;

let getsockname = unary_fd Unix.getsockname

let getpeername = unary_fd Unix.getpeername

type msg_flag =
Unix.msg_flag =
| MSG_OOB
| MSG_DONTROUTE
| MSG_PEEK
[@@deriving sexp]

let recv_send f fd ~buf ~pos ~len ~mode =
  improve (fun () -> f fd ~buf ~pos ~len ~mode)
    (fun () ->
      [fd_r fd;
       ("pos", Int.sexp_of_t pos);
       len_r len;
       ("mode", sexp_of_list sexp_of_msg_flag mode)])
;;

let recv = recv_send Unix.recv
let recvfrom = recv_send Unix.recvfrom
let send = recv_send Unix.send

let sendto fd ~buf ~pos ~len ~mode ~addr =
  improve (fun () -> Unix.sendto fd ~buf ~pos ~len ~mode ~addr)
    (fun () ->
      [fd_r fd;
       ("pos", Int.sexp_of_t pos);
       len_r len;
       ("mode", sexp_of_list sexp_of_msg_flag mode);
       ("addr", sexp_of_sockaddr addr)])
;;

type socket_bool_option = Unix.socket_bool_option =
  | SO_DEBUG
  | SO_BROADCAST
  | SO_REUSEADDR
  | SO_KEEPALIVE
  | SO_DONTROUTE
  | SO_OOBINLINE
  | SO_ACCEPTCONN
  | TCP_NODELAY
  | IPV6_ONLY
[@@deriving sexp]

type socket_int_option = Unix.socket_int_option =
  | SO_SNDBUF
  | SO_RCVBUF
  | SO_ERROR
  | SO_TYPE
  | SO_RCVLOWAT
  | SO_SNDLOWAT
[@@deriving sexp]

type socket_optint_option = Unix.socket_optint_option =
  | SO_LINGER
[@@deriving sexp]

type socket_float_option = Unix.socket_float_option =
  | SO_RCVTIMEO
  | SO_SNDTIMEO
[@@deriving sexp]

let make_sockopt get set sexp_of_opt sexp_of_val =
  let getsockopt fd opt =
    improve (fun () -> get fd opt)
      (fun () -> [fd_r fd; ("opt", sexp_of_opt opt)])
  in
  let setsockopt fd opt value =
    improve (fun () -> set fd opt value)
      (fun () ->
        [fd_r fd; ("opt", sexp_of_opt opt); ("val", sexp_of_val value)])
  in
  (getsockopt, setsockopt)
;;

let (getsockopt, setsockopt) =
  make_sockopt Unix.getsockopt Unix.setsockopt
    sexp_of_socket_bool_option sexp_of_bool
;;

let (getsockopt_int, setsockopt_int) =
  make_sockopt Unix.getsockopt_int Unix.setsockopt_int
    sexp_of_socket_int_option sexp_of_int
;;

let (getsockopt_optint, setsockopt_optint) =
  make_sockopt Unix.getsockopt_optint Unix.setsockopt_optint
    sexp_of_socket_optint_option (sexp_of_option sexp_of_int)
;;

let (getsockopt_float, setsockopt_float) =
  make_sockopt Unix.getsockopt_float Unix.setsockopt_float
    sexp_of_socket_float_option sexp_of_float
;;

(* Additional IP functionality *)

external if_indextoname : int -> string = "unix_if_indextoname"

module Mcast_action = struct
  (* Keep this in sync with the VAL_MCAST_ACTION_* #defines in unix_stubs.c *)
  type t =
    | Add
    | Drop
end

external mcast_modify
  :  Mcast_action.t
  -> ?ifname : string
  -> ?source : Inet_addr.t
  -> File_descr.t
  -> Unix.sockaddr
  -> unit
  = "core_unix_mcast_modify"
;;

let mcast_join ?ifname ?source fd sockaddr =
  mcast_modify Mcast_action.Add ?ifname ?source fd sockaddr
;;

let mcast_leave ?ifname fd sockaddr =
  mcast_modify Mcast_action.Drop ?ifname fd sockaddr
;;

external get_mcast_ttl : File_descr.t -> int = "unix_mcast_get_ttl"

external set_mcast_ttl : File_descr.t -> int -> unit = "unix_mcast_set_ttl"

external get_mcast_loop : File_descr.t -> bool = "unix_mcast_get_loop"

external set_mcast_loop : File_descr.t -> bool -> unit = "unix_mcast_set_loop"

external set_mcast_ifname : File_descr.t -> string -> unit = "unix_mcast_set_ifname"

let open_connection addr =
  improve (fun () -> Unix.open_connection addr) (fun () -> [addr_r addr])
;;

let shutdown_connection = Unix.shutdown_connection

let establish_server handle_connection ~addr =
  improve (fun () -> Unix.establish_server handle_connection ~addr)
    (fun () -> [addr_r addr])
;;

type addr_info = Unix.addr_info =
  { ai_family    : socket_domain
  ; ai_socktype  : socket_type
  ; ai_protocol  : int
  ; ai_addr      : sockaddr
  ; ai_canonname : string
  } [@@deriving sexp_of]

type addr_info_blocking_sexp = Unix.addr_info =
  { ai_family    : socket_domain
  ; ai_socktype  : socket_type
  ; ai_protocol  : int
  ; ai_addr      : sockaddr_blocking_sexp
  ; ai_canonname : string
  } [@@deriving sexp]

type getaddrinfo_option = Unix.getaddrinfo_option =
  | AI_FAMILY of socket_domain
  | AI_SOCKTYPE of socket_type
  | AI_PROTOCOL of int
  | AI_NUMERICHOST
  | AI_CANONNAME
  | AI_PASSIVE
[@@deriving sexp]

let getaddrinfo host service opts =
  improve (fun () -> Unix.getaddrinfo host service opts)
    (fun () ->
      [("host", atom host);
       ("service", atom service);
       ("opts", sexp_of_list sexp_of_getaddrinfo_option opts)])
;;

type name_info =
Unix.name_info = {
  ni_hostname : string;
  ni_service : string;
}
[@@deriving sexp]

type getnameinfo_option =
Unix.getnameinfo_option =
| NI_NOFQDN
| NI_NUMERICHOST
| NI_NAMEREQD
| NI_NUMERICSERV
| NI_DGRAM
[@@deriving sexp]

let getnameinfo addr opts =
  improve (fun () -> Unix.getnameinfo addr opts)
    (fun () ->
      [("addr", sexp_of_sockaddr addr);
       ("opts", sexp_of_list sexp_of_getnameinfo_option opts)])
;;

module Terminal_io = struct
  type t = Unix.terminal_io = {
    mutable c_ignbrk : bool;
    mutable c_brkint : bool;
    mutable c_ignpar : bool;
    mutable c_parmrk : bool;
    mutable c_inpck : bool;
    mutable c_istrip : bool;
    mutable c_inlcr : bool;
    mutable c_igncr : bool;
    mutable c_icrnl : bool;
    mutable c_ixon : bool;
    mutable c_ixoff : bool;
    mutable c_opost : bool;
    mutable c_obaud : int;
    mutable c_ibaud : int;
    mutable c_csize : int;
    mutable c_cstopb : int;
    mutable c_cread : bool;
    mutable c_parenb : bool;
    mutable c_parodd : bool;
    mutable c_hupcl : bool;
    mutable c_clocal : bool;
    mutable c_isig : bool;
    mutable c_icanon : bool;
    mutable c_noflsh : bool;
    mutable c_echo : bool;
    mutable c_echoe : bool;
    mutable c_echok : bool;
    mutable c_echonl : bool;
    mutable c_vintr : char;
    mutable c_vquit : char;
    mutable c_verase : char;
    mutable c_vkill : char;
    mutable c_veof : char;
    mutable c_veol : char;
    mutable c_vmin : int;
    mutable c_vtime : int;
    mutable c_vstart : char;
    mutable c_vstop : char;
  }
  [@@deriving sexp]

  let tcgetattr = unary_fd Unix.tcgetattr

  type setattr_when = Unix.setattr_when =
    | TCSANOW
    | TCSADRAIN
    | TCSAFLUSH
  [@@deriving sexp]

  let tcsetattr t fd ~mode =
    improve (fun () -> Unix.tcsetattr fd ~mode t)
      (fun () -> [fd_r fd;
                  ("mode", sexp_of_setattr_when mode);
                  ("termios", sexp_of_t t)])
  ;;

  let tcsendbreak fd ~duration =
    improve (fun () -> Unix.tcsendbreak fd ~duration)
      (fun () -> [fd_r fd;
                  ("duration", Int.sexp_of_t duration)])
  ;;

  let tcdrain = unary_fd Unix.tcdrain

  type flush_queue = Unix.flush_queue =
    | TCIFLUSH
    | TCOFLUSH
    | TCIOFLUSH
  [@@deriving sexp]

  let tcflush fd ~mode =
    improve (fun () -> Unix.tcflush fd ~mode)
      (fun () -> [fd_r fd; ("mode", sexp_of_flush_queue mode)])
  ;;

  type flow_action = Unix.flow_action =
    | TCOOFF
    | TCOON
    | TCIOFF
    | TCION
  [@@deriving sexp]

  let tcflow fd ~mode =
    improve (fun () -> Unix.tcflow fd ~mode)
      (fun () -> [fd_r fd; ("mode", sexp_of_flow_action mode)])
  ;;

  let setsid = Unix.setsid
end

let get_sockaddr name port = ADDR_INET (Inet_addr.of_string_or_getbyname name, port)

let set_in_channel_timeout ic rcv_timeout =
  let s = descr_of_in_channel ic in
  setsockopt_float s SO_RCVTIMEO rcv_timeout

let set_out_channel_timeout oc snd_timeout =
  let s = descr_of_out_channel oc in
  setsockopt_float s SO_SNDTIMEO snd_timeout

external nanosleep : float -> float = "core_time_ns_nanosleep" ;;

module Syslog = Syslog

let () = Sexplib_unix.Sexplib_unix_conv.linkme

(* Test the Sexplib_unix exn converter was added correctly *)
let%test_unit "Sexplib_unix sexp converter" =
  let open Sexp.O in
  match sexp_of_exn (Unix.Unix_error (E2BIG, "loc", "arg")) with
  | (List [ Atom "Unix.Unix_error"
          ; Atom _human_readable_message
          ; Atom "loc"
          ; Atom "arg"
          ]) -> ()
  | something_else ->
      failwithf "sexp_of_exn (Unix_error ...) gave %s" (Sexp.to_string something_else) ()
;;

module Ifaddr = struct
  module Broadcast_or_destination = struct
    type t =
      | Broadcast   of Inet_addr.t
      | Destination of Inet_addr.t
    [@@deriving sexp_of]
  end

  (* THE ORDER OF THESE IS IMPORTANT, SEE unix_stubs.c!!! *)
  module Family = struct
    type t = Packet | Inet4 | Inet6 [@@deriving sexp, bin_io]
  end

  module Flag = struct
    (* THE ORDER OF FLAGS IS IMPORTANT TO MATCH unix_stubs.c!!! *)
    module T = struct
      type t =
        | Allmulti
        | Automedia
        | Broadcast
        | Debug
        | Dynamic
        | Loopback
        | Master
        | Multicast
        | Noarp
        | Notrailers
        | Pointopoint
        | Portsel
        | Promisc
        | Running
        | Slave
        | Up
      [@@deriving sexp, compare, enumerate]
    end
    include T
    include Comparable.Make(T)

    external core_unix_iff_to_int : t -> int  = "core_unix_iff_to_int"

    let set_of_int bitmask =
      List.fold all
        ~init:Set.empty
        ~f:(fun flags t ->
          let v = core_unix_iff_to_int t in
          match bitmask land v with
          | 0 -> flags
          | _ -> Set.add flags t)
    ;;

    let int_of_set =
      Set.fold ~init:0 ~f:(fun acc t -> acc lor (core_unix_iff_to_int t))
    ;;

    let to_int = core_unix_iff_to_int

    let%test_unit _ = [%test_result: Set.t] (set_of_int 0) ~expect:Set.empty

    let%test_unit _ =
      List.iter all ~f:(fun t ->
        let x = to_int t in
        if Int.(<>) (Int.ceil_pow2 x) x
        then failwiths "Flag is not a power of 2" t sexp_of_t)

    let%test_unit _ =
      List.iter all ~f:(fun t ->
        [%test_result: Set.t] (set_of_int (int_of_set (Set.singleton t)))
          ~expect:(Set.singleton t))

    let%test_unit _ =
      [%test_result: Set.t] (set_of_int (int_of_set (Set.of_list all)))
        ~expect:(Set.of_list all)
  end

  type t =
    { name                     : string
    ; family                   : Family.t
    ; flags                    : Flag.Set.t
    ; address                  : Inet_addr.t                sexp_option
    ; netmask                  : Inet_addr.t                sexp_option
    ; broadcast_or_destination : Broadcast_or_destination.t sexp_option
    }
  [@@deriving sexp_of, fields]

  (* THE ORDER AND NUMBER OF THESE IS IMPORTANT, SEE unix_stubs.c!!! *)
  type ifaddrs =
    { name               : string
    ; family             : Family.t
    ; flags              : int
    ; addr_octets        : string
    ; netmask_octets     : string
    ; broadcast_octets   : string
    ; destination_octets : string
    }
  external core_unix_getifaddrs : unit -> ifaddrs list = "core_unix_getifaddrs"

  let inet4_to_inet_addr addr =
    match String.length addr with
    | 0 -> None
    | 4 ->
      sprintf "%d.%d.%d.%d"
        (Char.to_int addr.[0])
        (Char.to_int addr.[1])
        (Char.to_int addr.[2])
        (Char.to_int addr.[3])
      |> Inet_addr.of_string
      |> Option.return
    | addrlen -> failwithf "IPv4 address is length %d!" addrlen ()
  ;;

  let inet6_to_inet_addr addr =
    match String.length addr with
    | 0  -> None
    | 16 ->
      sprintf "%02x%02x:%02x%02x:%02x%02x:%02x%02x:%02x%02x:%02x%02x:%02x%02x:%02x%02x"
        (Char.to_int addr.[ 0])
        (Char.to_int addr.[ 1])
        (Char.to_int addr.[ 2])
        (Char.to_int addr.[ 3])
        (Char.to_int addr.[ 4])
        (Char.to_int addr.[ 5])
        (Char.to_int addr.[ 6])
        (Char.to_int addr.[ 7])
        (Char.to_int addr.[ 8])
        (Char.to_int addr.[ 9])
        (Char.to_int addr.[10])
        (Char.to_int addr.[11])
        (Char.to_int addr.[12])
        (Char.to_int addr.[13])
        (Char.to_int addr.[14])
        (Char.to_int addr.[15])
      |> Inet_addr.of_string
      |> Option.return
    | addrlen -> failwithf "IPv6 address is length %d!" addrlen ()
  ;;

  let addr_to_inet_addr family addr =
    match family with
    | Family.Packet -> None
    | Family.Inet4  -> inet4_to_inet_addr addr
    | Family.Inet6  -> inet6_to_inet_addr addr
  ;;

  let test_and_convert ifa =
    let flags = Flag.set_of_int ifa.flags in
    let broadcast_or_destination_convert ifa =
      if Set.mem flags Broadcast
      then
        Option.map (addr_to_inet_addr ifa.family ifa.broadcast_octets)
          ~f:(fun x -> Broadcast_or_destination.Broadcast x)
      else if Set.mem flags Pointopoint
      then
        Option.map (addr_to_inet_addr ifa.family ifa.destination_octets)
          ~f:(fun x -> Broadcast_or_destination.Destination x)
      else None
    in
    { address                  = addr_to_inet_addr ifa.family ifa.addr_octets
    ; netmask                  = addr_to_inet_addr ifa.family ifa.netmask_octets
    ; broadcast_or_destination = broadcast_or_destination_convert ifa
    ; flags                    = flags
    ; name                     = ifa.name
    ; family                   = ifa.family
    }
  ;;
end

let getifaddrs () =
  List.map (Ifaddr.core_unix_getifaddrs ()) ~f:Ifaddr.test_and_convert
;;

module Stable = struct
  module Inet_addr = Inet_addr.Stable
  module Cidr = Cidr.Stable
end
