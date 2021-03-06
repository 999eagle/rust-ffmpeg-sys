extern crate bindgen;
extern crate cc;
extern crate num_cpus;
extern crate pkg_config;
extern crate regex;

use std::env;
use std::fs::{self, create_dir, symlink_metadata, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::str;

use bindgen::callbacks::{IntKind, MacroParsingBehavior, ParseCallbacks};
use regex::Regex;

#[derive(Debug)]
struct IntCallbacks;

impl ParseCallbacks for IntCallbacks {
    fn int_macro(&self, _name: &str, value: i64) -> Option<IntKind> {
        let ch_layout = Regex::new(r"^AV_CH").unwrap();
        let codec_cap = Regex::new(r"^AV_CODEC_CAP").unwrap();
        let codec_flag = Regex::new(r"^AV_CODEC_FLAG").unwrap();
        let error_max_size = Regex::new(r"^AV_ERROR_MAX_STRING_SIZE").unwrap();

        if value >= i64::min_value() as i64 && value <= i64::max_value() as i64
            && ch_layout.is_match(_name)
        {
            Some(IntKind::ULongLong)
        } else if value >= i32::min_value() as i64 && value <= i32::max_value() as i64
            && (codec_cap.is_match(_name) || codec_flag.is_match(_name))
        {
            Some(IntKind::UInt)
        } else if error_max_size.is_match(_name) {
            Some(IntKind::Custom {
                name: "usize",
                is_signed: false,
            })
        } else if value >= i32::min_value() as i64 && value <= i32::max_value() as i64 {
            Some(IntKind::Int)
        } else {
            None
        }
    }

    fn will_parse_macro(&self, name: &str) -> MacroParsingBehavior {
        match name {
            "FP_NAN" | "FP_INFINITE" | "FP_ZERO" | "FP_SUBNORMAL" | "FP_NORMAL" => {
                MacroParsingBehavior::Ignore
            }
            _ => MacroParsingBehavior::Default,
        }
    }
}

fn version() -> String {
    let major: u8 = env::var("CARGO_PKG_VERSION_MAJOR")
        .unwrap()
        .parse()
        .unwrap();
    let minor: u8 = env::var("CARGO_PKG_VERSION_MINOR")
        .unwrap()
        .parse()
        .unwrap();

    format!("{}.{}", major, minor)
}

fn output() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap())
}

fn source() -> PathBuf {
    output().join(format!("ffmpeg-{}", version()))
}

fn search() -> PathBuf {
    let mut absolute = env::current_dir().unwrap();
    absolute.push(&output());
    absolute.push("dist");

    absolute
}

fn fetch() -> io::Result<()> {
    println!("Fetch FFmpeg Version {:?} from Git", version());

    let target = output().join(format!("ffmpeg-{}", version()));
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    let status = Command::new("git")
        .current_dir(&output())
        .arg("clone")
        .arg("-b")
        .arg(format!("release/{}", version()))
        .arg("https://github.com/FFmpeg/FFmpeg")
        .arg(format!("ffmpeg-{}", version()))
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "fetch failed"))
    }
}

fn build() -> io::Result<()> {
    println!("Start build");
    let mut args = Vec::new();

    if env::var("TARGET").unwrap().contains("windows") {
        let target = env::var("TARGET").unwrap();
        if target.contains("-msvc") {
            args.push("--toolchain=msvc".into());
        }
        if target.contains("x86_64") {
            args.push("--target-os=win64".into());
            args.push("--arch=x86_64".into());
        }
        args.push(format!(
            "--prefix=/{}",
            search()
                .to_string_lossy()
                .replace(':', "")
                .replace('\\', "/")
                .replace(' ', "\\ ")
                .replace('"', "\\\"")
        ));
    } else {
        args.push(format!("--prefix={}", search().to_string_lossy()));
    }

    if env::var("TARGET").unwrap() != env::var("HOST").unwrap() {
        args.push(format!("--cross-prefix={}-", env::var("TARGET").unwrap()));
    }

    // control debug build
    if env::var("DEBUG").is_ok() {
        args.push("--enable-debug".into());
        args.push("--disable-stripping".into());
    } else {
        args.push("--disable-debug".into());
        args.push("--enable-stripping".into());
    }

    // make it static
    args.push("--enable-static".into());
    args.push("--disable-shared".into());

    args.push("--enable-pic".into());

    macro_rules! switch {
        ($conf:expr, $feat:expr, $name:expr) => {
            if env::var(concat!("CARGO_FEATURE_", $feat)).is_ok() {
                $conf.push(concat!("--enable-", $name).into());
            } else {
                $conf.push(concat!("--disable-", $name).into());
            }
        };
    }

    macro_rules! enable {
        ($conf:expr, $feat:expr, $name:expr) => {
            if env::var(concat!("CARGO_FEATURE_", $feat)).is_ok() {
                $conf.push(concat!("--enable-", $name).into());
            }
        };
    }

    // macro_rules! disable {
    //     ($conf:expr, $feat:expr, $name:expr) => (
    //         if env::var(concat!("CARGO_FEATURE_", $feat)).is_err() {
    //             $conf.arg(concat!("--disable-", $name));
    //         }
    //     )
    // }

    // the binary using ffmpeg-sys must comply with GPL
    switch!(args, "BUILD_LICENSE_GPL", "gpl");

    // the binary using ffmpeg-sys must comply with (L)GPLv3
    switch!(args, "BUILD_LICENSE_VERSION3", "version3");

    // the binary using ffmpeg-sys cannot be redistributed
    switch!(args, "BUILD_LICENSE_NONFREE", "nonfree");

    // configure building libraries based on features
    switch!(args, "AVCODEC", "avcodec");
    switch!(args, "AVDEVICE", "avdevice");
    switch!(args, "AVFILTER", "avfilter");
    switch!(args, "AVFORMAT", "avformat");
    switch!(args, "AVRESAMPLE", "avresample");
    switch!(args, "POSTPROC", "postproc");
    switch!(args, "SWRESAMPLE", "swresample");
    switch!(args, "SWSCALE", "swscale");

    // configure building programs based on features
    switch!(args, "FFMPEG", "ffmpeg");
    switch!(args, "FFPLAY", "ffplay");
    switch!(args, "FFPROBE", "ffprobe");

    // configure external SSL libraries
    enable!(args, "BUILD_LIB_GNUTLS", "gnutls");
    enable!(args, "BUILD_LIB_OPENSSL", "openssl");
    enable!(args, "BUILD_LIB_SCHANNEL", "schannel");
    enable!(args, "BUILD_LIB_SECURETRANSPORT", "securetransport");

    // configure external filters
    enable!(args, "BUILD_LIB_FONTCONFIG", "fontconfig");
    enable!(args, "BUILD_LIB_FREI0R", "frei0r");
    enable!(args, "BUILD_LIB_LADSPA", "ladspa");
    enable!(args, "BUILD_LIB_ASS", "libass");
    enable!(args, "BUILD_LIB_FREETYPE", "libfreetype");
    enable!(args, "BUILD_LIB_FRIBIDI", "libfribidi");
    enable!(args, "BUILD_LIB_OPENCV", "libopencv");

    // configure external encoders/decoders
    enable!(args, "BUILD_LIB_AACPLUS", "libaacplus");
    enable!(args, "BUILD_LIB_CELT", "libcelt");
    enable!(args, "BUILD_LIB_DCADEC", "libdcadec");
    enable!(args, "BUILD_LIB_FAAC", "libfaac");
    enable!(args, "BUILD_LIB_FDK_AAC", "libfdk-aac");
    enable!(args, "BUILD_LIB_GSM", "libgsm");
    enable!(args, "BUILD_LIB_ILBC", "libilbc");
    enable!(args, "BUILD_LIB_VAZAAR", "libvazaar");
    enable!(args, "BUILD_LIB_MP3LAME", "libmp3lame");
    enable!(args, "BUILD_LIB_OPENCORE_AMRNB", "libopencore-amrnb");
    enable!(args, "BUILD_LIB_OPENCORE_AMRWB", "libopencore-amrwb");
    enable!(args, "BUILD_LIB_OPENH264", "libopenh264");
    enable!(args, "BUILD_LIB_OPENH265", "libopenh265");
    enable!(args, "BUILD_LIB_OPENJPEG", "libopenjpeg");
    enable!(args, "BUILD_LIB_OPUS", "libopus");
    enable!(args, "BUILD_LIB_SCHROEDINGER", "libschroedinger");
    enable!(args, "BUILD_LIB_SHINE", "libshine");
    enable!(args, "BUILD_LIB_SNAPPY", "libsnappy");
    enable!(args, "BUILD_LIB_SPEEX", "libspeex");
    enable!(args, "BUILD_LIB_STAGEFRIGHT_H264", "libstagefright-h264");
    enable!(args, "BUILD_LIB_THEORA", "libtheora");
    enable!(args, "BUILD_LIB_TWOLAME", "libtwolame");
    enable!(args, "BUILD_LIB_UTVIDEO", "libutvideo");
    enable!(args, "BUILD_LIB_VO_AACENC", "libvo-aacenc");
    enable!(args, "BUILD_LIB_VO_AMRWBENC", "libvo-amrwbenc");
    enable!(args, "BUILD_LIB_VORBIS", "libvorbis");
    enable!(args, "BUILD_LIB_VPX", "libvpx");
    enable!(args, "BUILD_LIB_WAVPACK", "libwavpack");
    enable!(args, "BUILD_LIB_WEBP", "libwebp");
    enable!(args, "BUILD_LIB_X264", "libx264");
    enable!(args, "BUILD_LIB_X265", "libx265");
    enable!(args, "BUILD_LIB_AVS", "libavs");
    enable!(args, "BUILD_LIB_XVID", "libxvid");

    // other external libraries
    enable!(args, "BUILD_NVENC", "nvenc");

    // configure external protocols
    enable!(args, "BUILD_LIB_SMBCLIENT", "libsmbclient");
    enable!(args, "BUILD_LIB_SSH", "libssh");

    // configure misc build options
    enable!(args, "BUILD_PIC", "pic");

    let mut configure = if env::var("TARGET").unwrap().contains("windows") {
        let mut arg = String::from("./configure ");
        arg.push_str(&args.join(" "));
        let mut configure = Command::new("sh");
        configure.arg("-c").arg(arg);
        configure
    } else {
        let mut configure = Command::new("./configure");
        configure.args(args);
        configure
    };
    configure.current_dir(&source());

    // run ./configure
    let output = configure
        .output()
        .expect(&format!("{:?} failed", configure));
    if !output.status.success() {
        println!("configure: {}", String::from_utf8_lossy(&output.stdout));

        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "configure failed {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }

    // run make
    if !try!(
        Command::new("make")
            .arg("-j")
            .arg(num_cpus::get().to_string())
            .current_dir(&source())
            .status()
    ).success()
    {
        return Err(io::Error::new(io::ErrorKind::Other, "make failed"));
    }

    // run make install
    if !try!(
        Command::new("make")
            .current_dir(&source())
            .arg("install")
            .status()
    ).success()
    {
        return Err(io::Error::new(io::ErrorKind::Other, "make install failed"));
    }

    Ok(())
}

fn check_features(
    include_paths: Vec<PathBuf>,
    infos: &Vec<(&'static str, Option<&'static str>, &'static str)>,
) {
    let mut includes_code = String::new();
    let mut main_code = String::new();

    for &(header, feature, var) in infos {
        if let Some(feature) = feature {
            if env::var(format!("CARGO_FEATURE_{}", feature.to_uppercase())).is_err() {
                continue;
            }
        }

        let include = format!("#include <{}>", header);
        if includes_code.find(&include).is_none() {
            includes_code.push_str(&include);
            includes_code.push_str(&"\n");
        }
        includes_code.push_str(&format!(
            r#"
            #ifndef {var}
            #define {var} 0
            #define {var}_is_defined 0
            #else
            #define {var}_is_defined 1
            #endif
        "#,
            var = var
        ));

        main_code.push_str(&format!(
            r#"printf("[{var}]%d%d\n", {var}, {var}_is_defined);"#,
            var = var
        ));
    }

    let version_check_info = [("avcodec", 56, 60, 0, 80)];
    for &(lib, begin_version_major, end_version_major, begin_version_minor, end_version_minor) in
        version_check_info.iter()
    {
        for version_major in begin_version_major..end_version_major {
            for version_minor in begin_version_minor..end_version_minor {
                main_code.push_str(&format!(
                    r#"printf("[{lib}_version_greater_than_{version_major}_{version_minor}]%d\n", LIB{lib_uppercase}_VERSION_MAJOR > {version_major} || (LIB{lib_uppercase}_VERSION_MAJOR == {version_major} && LIB{lib_uppercase}_VERSION_MINOR > {version_minor}));"#,
                    lib = lib,
                    lib_uppercase = lib.to_uppercase(),
                    version_major = version_major,
                    version_minor = version_minor
                ));
            }
        }
    }

    let out_dir = output();

    write!(
        File::create(out_dir.join("check.c")).expect("Failed to create file"),
        r#"
            #include <stdio.h>
            {includes_code}

            int main()
            {{
                {main_code}
                return 0;
            }}
           "#,
        includes_code = includes_code,
        main_code = main_code
    ).expect("Write failed");

    let executable = out_dir.join(if cfg!(windows) { "check.exe" } else { "check" });
    let mut compiler = cc::Build::new().get_compiler().to_command();

    for dir in include_paths {
        compiler.arg("-I");
        compiler.arg(dir.to_string_lossy().into_owned());
    }
    if !compiler
        .current_dir(&out_dir)
        .arg("-o")
        .arg(&executable)
        .arg("check.c")
        .status()
        .expect("Command failed")
        .success()
    {
        panic!("Compile failed");
    }

    let stdout_raw = Command::new(out_dir.join(&executable))
        .current_dir(&out_dir)
        .output()
        .expect("Check failed")
        .stdout;
    let stdout = str::from_utf8(stdout_raw.as_slice()).unwrap();

    //let mut f = File::open("check.txt").expect("check.txt file not found!");

    /*let mut stdout = String::new();
    f.read_to_string(&mut stdout)
        .expect("something went wrong reading the file");*/

    //let stdout = contents.unwrap();

    //println!("use check.txt test...");

    println!("stdout={}", stdout);

    for &(_, feature, var) in infos {
        if let Some(feature) = feature {
            if env::var(format!("CARGO_FEATURE_{}", feature.to_uppercase())).is_err() {
                continue;
            }
        }

        let var_str = format!("[{var}]", var = var);
        let pos = stdout.find(&var_str).expect("H-Variable not found in output") + var_str.len();
        if &stdout[pos..pos + 1] == "1" {
            println!(r#"cargo:rustc-cfg=feature="{}""#, var.to_lowercase());
            println!(r#"cargo:{}=true"#, var.to_lowercase());
        }

        // Also find out if defined or not (useful for cases where only the definition of a macro
        // can be used as distinction)
        if &stdout[pos + 1..pos + 2] == "1" {
            println!(
                r#"cargo:rustc-cfg=feature="{}_is_defined""#,
                var.to_lowercase()
            );
            println!(r#"cargo:{}_is_defined=true"#, var.to_lowercase());
        }
    }

    for &(lib, begin_version_major, end_version_major, begin_version_minor, end_version_minor) in
        version_check_info.iter()
    {
        for version_major in begin_version_major..end_version_major {
            for version_minor in begin_version_minor..end_version_minor {
                let search_str = format!(
                    "[{lib}_version_greater_than_{version_major}_{version_minor}]",
                    version_major = version_major,
                    version_minor = version_minor,
                    lib = lib
                );
                let pos = stdout
                    .find(&search_str)
                    .expect("Variable not found in output")
                    + search_str.len();

                if &stdout[pos..pos + 1] == "1" {
                    println!(
                        r#"cargo:rustc-cfg=feature="{}""#,
                        &search_str[1..(search_str.len() - 1)]
                    );
                }
            }
        }
    }
}

fn search_include(include_paths: &Vec<PathBuf>, header: &str) -> String {
    for dir in include_paths {
        let include = dir.join(header);
        if fs::metadata(&include).is_ok() {
            return format!("{}", include.as_path().to_str().unwrap());
        }
    }
    format!("/usr/include/{}", header)
}

fn main() {
    let statik = env::var("CARGO_FEATURE_STATIC").is_ok();

    let include_paths: Vec<PathBuf> = if env::var("CARGO_FEATURE_BUILD").is_ok() {
        println!(
            "cargo:rustc-link-search=native={}",
            search().join("lib").to_string_lossy()
        );
        println!("FFMPEG-SYS get build...");
        let ffmpeg_ty = if statik { "static" } else { "dylib" };

        // Make sure to link with the ffmpeg libs we built
        println!("cargo:rustc-link-lib={}=avutil", ffmpeg_ty);
        if env::var("CARGO_FEATURE_AVCODEC").is_ok() {
            println!("cargo:rustc-link-lib={}=avcodec", ffmpeg_ty);
        }
        if env::var("CARGO_FEATURE_AVFORMAT").is_ok() {
            println!("cargo:rustc-link-lib={}=avformat", ffmpeg_ty);
        }
        if env::var("CARGO_FEATURE_AVFILTER").is_ok() {
            println!("cargo:rustc-link-lib={}=avfilter", ffmpeg_ty);
        }
        if env::var("CARGO_FEATURE_AVDEVICE").is_ok() {
            println!("cargo:rustc-link-lib={}=avdevice", ffmpeg_ty);
        }
        if env::var("CARGO_FEATURE_AVRESAMPLE").is_ok() {
            println!("cargo:rustc-link-lib={}=avresample", ffmpeg_ty);
        }
        if env::var("CARGO_FEATURE_SWSCALE").is_ok() {
            println!("cargo:rustc-link-lib={}=swscale", ffmpeg_ty);
        }
        if env::var("CARGO_FEATURE_SWRESAMPLE").is_ok() {
            println!("cargo:rustc-link-lib={}=swresample", ffmpeg_ty);
        }

        if env::var("CARGO_FEATURE_BUILD_ZLIB").is_ok() && cfg!(target_os = "linux") {
            println!("cargo:rustc-link-lib=z");
        }

        if fs::metadata(&search().join("lib").join("libavutil.a")).is_err() {
            fs::create_dir_all(&output())
                .ok()
                .expect("failed to create build directory");
            fetch().unwrap();
            build().unwrap();
        }

        // Check additional required libraries.
        {
            let config_mak = source().join("ffbuild/config.mak");
            let file = File::open(config_mak).unwrap();
            let reader = BufReader::new(file);

            let mut include_libs = Vec::new();
            for line in reader.lines() {
                if !line.as_ref().unwrap().starts_with("EXTRALIBS") {
                    continue;
                }
                let line = line.unwrap();
                let mut split = line.splitn(2, '=');
                let lib = split.next().unwrap().split('-').last().unwrap();
                let linker_args = split.next().unwrap();

                // the key EXTRALIBS on its own should be linked unconditionally
                // avutil is always built
                if lib != "EXTRALIBS" && lib != "avutil" {
                    // check feature flag if we need to link these libs
                    let feature = format!("CARGO_FEATURE_{}", lib.to_uppercase());
                    if env::var(feature).is_err() {
                        continue;
                    }
                }

                let libs: Vec<_> = if env::var("TARGET").unwrap().contains("windows") {
                    linker_args
                        .split(' ')
                        .filter(|v| v.ends_with(".lib"))
                        .map(|lib| &lib[..lib.len() - 4])
                        .map(|lib| lib.to_owned())
                        .collect()
                } else {
                    linker_args
                        .split(' ')
                        .filter(|v| v.starts_with("-l"))
                        .map(|flag| &flag[2..])
                        .map(|lib| lib.to_owned())
                        .collect()
                };
                for lib in libs {
                    if !include_libs.contains(&lib) {
                        include_libs.push(lib);
                    }
                }
            }

            for lib in include_libs {
                println!("cargo:rustc-link-lib={}", lib);
            }
        }

        // copy binaries to output
        {
            let binaries = vec![
                ("ffmpeg", "FFMPEG"),
                ("ffplay", "FFPLAY"),
                ("ffprobe", "FFPROBE"),
            ];
            for (bin, feature) in binaries {
                if env::var(format!("CARGO_FEATURE_{}", feature)).is_ok() {
                    let bin = if env::var("TARGET").unwrap().contains("windows") {
                        PathBuf::from(bin).with_extension("exe")
                    } else {
                        PathBuf::from(bin)
                    };
                    let bin_path = search().join("bin").join(&bin);
                    let out_path = output()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join(&bin);
                    if out_path.exists() {
                        fs::remove_file(&out_path)
                            .expect(&format!("failed to remove {}", out_path.to_string_lossy()));
                    }
                    fs::copy(&bin_path, &out_path).expect(&format!(
                        "failed to copy {} to {}",
                        bin_path.to_string_lossy(),
                        out_path.to_string_lossy()
                    ));
                }
            }
        }

        vec![search().join("include")]
    }
    // Use prebuilt library
    else if let Ok(ffmpeg_dir) = env::var("FFMPEG_DIR") {
        let ffmpeg_dir = PathBuf::from(ffmpeg_dir);

        println!(
            "cargo:rustc-link-search=native={}",
            ffmpeg_dir.join("lib").to_string_lossy()
        );

        vec![ffmpeg_dir.join("include")]
    }
    // Fallback to pkg-config
    else {
        println!("fallback to pkg-config");

        pkg_config::Config::new()
            .statik(statik)
            .probe("libavutil")
            .unwrap()
            .include_paths;

        let libs = vec![
            ("libavformat", "AVFORMAT"),
            ("libavfilter", "AVFILTER"),
            ("libavdevice", "AVDEVICE"),
            ("libavresample", "AVRESAMPLE"),
            ("libswscale", "SWSCALE"),
            ("libswresample", "SWRESAMPLE"),
        ];

        for (lib_name, env_variable_name) in libs.iter() {
            if env::var(format!("CARGO_FEATURE_{}", env_variable_name)).is_ok() {
                pkg_config::Config::new()
                    .statik(statik)
                    .probe(lib_name)
                    .unwrap()
                    .include_paths;
            }
        };

        pkg_config::Config::new()
            .statik(statik)
            .probe("libavcodec")
            .unwrap()
            .include_paths
    };

    if statik && cfg!(target_os = "macos") {
        let frameworks = vec![
            "AppKit",
            "AudioToolbox",
            "AVFoundation",
            "CoreFoundation",
            "CoreGraphics",
            "CoreMedia",
            "CoreServices",
            "CoreVideo",
            "Foundation",
            "OpenCL",
            "OpenGL",
            "QTKit",
            "QuartzCore",
            "Security",
            "VideoDecodeAcceleration",
            "VideoToolbox",
        ];
        for f in frameworks {
            println!("cargo:rustc-link-lib=framework={}", f);
        }
    }

    check_features(
        include_paths.clone(),
        &vec![
            ("libavutil/avutil.h", None, "FF_API_OLD_AVOPTIONS"),
            ("libavutil/avutil.h", None, "FF_API_PIX_FMT"),
            ("libavutil/avutil.h", None, "FF_API_CONTEXT_SIZE"),
            ("libavutil/avutil.h", None, "FF_API_PIX_FMT_DESC"),
            ("libavutil/avutil.h", None, "FF_API_AV_REVERSE"),
            ("libavutil/avutil.h", None, "FF_API_AUDIOCONVERT"),
            ("libavutil/avutil.h", None, "FF_API_CPU_FLAG_MMX2"),
            ("libavutil/avutil.h", None, "FF_API_LLS_PRIVATE"),
            ("libavutil/avutil.h", None, "FF_API_AVFRAME_LAVC"),
            ("libavutil/avutil.h", None, "FF_API_VDPAU"),
            (
                "libavutil/avutil.h",
                None,
                "FF_API_GET_CHANNEL_LAYOUT_COMPAT",
            ),
            ("libavutil/avutil.h", None, "FF_API_XVMC"),
            ("libavutil/avutil.h", None, "FF_API_OPT_TYPE_METADATA"),
            ("libavutil/avutil.h", None, "FF_API_DLOG"),
            ("libavutil/avutil.h", None, "FF_API_HMAC"),
            ("libavutil/avutil.h", None, "FF_API_VAAPI"),
            ("libavutil/avutil.h", None, "FF_API_PKT_PTS"),
            ("libavutil/avutil.h", None, "FF_API_ERROR_FRAME"),
            ("libavutil/avutil.h", None, "FF_API_FRAME_QP"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_VIMA_DECODER",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_REQUEST_CHANNELS",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_OLD_DECODE_AUDIO",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_OLD_ENCODE_AUDIO",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_OLD_ENCODE_VIDEO",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_CODEC_ID"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_AUDIO_CONVERT",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_AVCODEC_RESAMPLE",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_DEINTERLACE",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_DESTRUCT_PACKET",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_GET_BUFFER"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_MISSING_SAMPLE",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_LOWRES"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_CAP_VDPAU"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_BUFS_VDPAU"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_VOXWARE"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_SET_DIMENSIONS",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_DEBUG_MV"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_AC_VLC"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_OLD_MSMPEG4",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_ASPECT_EXTENDED",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_THREAD_OPAQUE",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_CODEC_PKT"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_ARCH_ALPHA"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_XVMC"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_ERROR_RATE"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_QSCALE_TYPE",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_MB_TYPE"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_MAX_BFRAMES",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_NEG_LINESIZES",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_EMU_EDGE"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_ARCH_SH4"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_ARCH_SPARC"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_UNUSED_MEMBERS",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_IDCT_XVIDMMX",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_INPUT_PRESERVED",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_NORMALIZE_AQP",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_GMC"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_MV0"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_CODEC_NAME"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_AFD"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_VISMV"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_DV_FRAME_PROFILE",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_AUDIOENC_DELAY",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_VAAPI_CONTEXT",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_AVCTX_TIMEBASE",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_MPV_OPT"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_STREAM_CODEC_TAG",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_QUANT_BIAS"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_RC_STRATEGY",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_CODED_FRAME",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_MOTION_EST"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_WITHOUT_PREFIX",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_CONVERGENCE_DURATION",
            ),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_PRIVATE_OPT",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_CODER_TYPE"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_RTP_CALLBACK",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_STAT_BITS"),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_VBV_DELAY"),
            (
                "libavcodec/avcodec.h",
                Some("avcodec"),
                "FF_API_SIDEDATA_ONLY_PKT",
            ),
            ("libavcodec/avcodec.h", Some("avcodec"), "FF_API_AVPICTURE"),
            (
                "libavformat/avformat.h",
                Some("avformat"),
                "FF_API_LAVF_BITEXACT",
            ),
            (
                "libavformat/avformat.h",
                Some("avformat"),
                "FF_API_LAVF_FRAC",
            ),
            (
                "libavformat/avformat.h",
                Some("avformat"),
                "FF_API_URL_FEOF",
            ),
            (
                "libavformat/avformat.h",
                Some("avformat"),
                "FF_API_PROBESIZE_32",
            ),
            (
                "libavformat/avformat.h",
                Some("avformat"),
                "FF_API_LAVF_AVCTX",
            ),
            (
                "libavformat/avformat.h",
                Some("avformat"),
                "FF_API_OLD_OPEN_CALLBACKS",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_AVFILTERPAD_PUBLIC",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_FOO_COUNT",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_OLD_FILTER_OPTS",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_OLD_FILTER_OPTS_ERROR",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_AVFILTER_OPEN",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_OLD_FILTER_REGISTER",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_OLD_GRAPH_PARSE",
            ),
            (
                "libavfilter/avfilter.h",
                Some("avfilter"),
                "FF_API_NOCONST_GET_NAME",
            ),
            (
                "libavresample/avresample.h",
                Some("avresample"),
                "FF_API_RESAMPLE_CLOSE_OPEN",
            ),
            (
                "libswscale/swscale.h",
                Some("swscale"),
                "FF_API_SWS_CPU_CAPS",
            ),
            ("libswscale/swscale.h", Some("swscale"), "FF_API_ARCH_BFIN"),
        ],
    );

    let tmp = std::env::current_dir().unwrap().join("tmp");
    if symlink_metadata(&tmp).is_err() {
        create_dir(&tmp).expect("Failed to create temporary output dir");
    }
    let mut f = File::create(tmp.join(".build")).expect("Filed to create .build");
    let tool = cc::Build::new().get_compiler();
    write!(f, "{}", tool.path().to_string_lossy().into_owned()).expect("failed to write cmd");
    for arg in tool.args() {
        write!(f, " {}", arg.to_str().unwrap()).expect("failed to write arg");
    }
    for dir in &include_paths {
        write!(f, " -I {}", dir.to_string_lossy().into_owned()).expect("failed to write incdir");
    }
    let clang_includes = include_paths
        .iter()
        .map(|include| format!("-I{}", include.to_string_lossy()));

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let mut builder = bindgen::Builder::default()
        .clang_args(clang_includes)
        .ctypes_prefix("libc")
        // https://github.com/servo/rust-bindgen/issues/687
        .blacklist_type("FP_NAN")
        .blacklist_type("FP_INFINITE")
        .blacklist_type("FP_ZERO")
        .blacklist_type("FP_SUBNORMAL")
        .blacklist_type("FP_NORMAL")
        // https://github.com/servo/rust-bindgen/issues/550
        .blacklist_type("max_align_t")
        .rustified_enum("*")
        .prepend_enum_name(false)
        .derive_eq(true)
        .parse_callbacks(Box::new(IntCallbacks));

    // The input headers we would like to generate
    // bindings for.
    if env::var("CARGO_FEATURE_AVCODEC").is_ok() {
        builder = builder
            .header(search_include(&include_paths, "libavcodec/avcodec.h"))
            .header(search_include(&include_paths, "libavcodec/dv_profile.h"))
            .header(search_include(&include_paths, "libavcodec/avfft.h"))
            .header(search_include(&include_paths, "libavcodec/vaapi.h"))
            .header(search_include(&include_paths, "libavcodec/vorbis_parser.h"));
    }

    if env::var("CARGO_FEATURE_AVDEVICE").is_ok() {
        builder = builder.header(search_include(&include_paths, "libavdevice/avdevice.h"));
    }

    if env::var("CARGO_FEATURE_AVFILTER").is_ok() {
        builder = builder
            .header(search_include(&include_paths, "libavfilter/buffersink.h"))
            .header(search_include(&include_paths, "libavfilter/buffersrc.h"))
            .header(search_include(&include_paths, "libavfilter/avfilter.h"));
    }

    if env::var("CARGO_FEATURE_AVFORMAT").is_ok() {
        builder = builder
            .header(search_include(&include_paths, "libavformat/avformat.h"))
            .header(search_include(&include_paths, "libavformat/avio.h"));
    }

    if env::var("CARGO_FEATURE_AVRESAMPLE").is_ok() {
        builder = builder.header(search_include(&include_paths, "libavresample/avresample.h"));
    }

    builder = builder
        .header(search_include(&include_paths, "libavutil/adler32.h"))
        .header(search_include(&include_paths, "libavutil/aes.h"))
        .header(search_include(&include_paths, "libavutil/audio_fifo.h"))
        .header(search_include(&include_paths, "libavutil/base64.h"))
        .header(search_include(&include_paths, "libavutil/blowfish.h"))
        .header(search_include(&include_paths, "libavutil/bprint.h"))
        .header(search_include(&include_paths, "libavutil/buffer.h"))
        .header(search_include(&include_paths, "libavutil/camellia.h"))
        .header(search_include(&include_paths, "libavutil/cast5.h"))
        .header(search_include(&include_paths, "libavutil/channel_layout.h"))
        .header(search_include(&include_paths, "libavutil/cpu.h"))
        .header(search_include(&include_paths, "libavutil/crc.h"))
        .header(search_include(&include_paths, "libavutil/dict.h"))
        .header(search_include(&include_paths, "libavutil/display.h"))
        .header(search_include(&include_paths, "libavutil/downmix_info.h"))
        .header(search_include(&include_paths, "libavutil/error.h"))
        .header(search_include(&include_paths, "libavutil/eval.h"))
        .header(search_include(&include_paths, "libavutil/fifo.h"))
        .header(search_include(&include_paths, "libavutil/file.h"))
        .header(search_include(&include_paths, "libavutil/frame.h"))
        .header(search_include(&include_paths, "libavutil/hash.h"))
        .header(search_include(&include_paths, "libavutil/hmac.h"))
        .header(search_include(&include_paths, "libavutil/imgutils.h"))
        .header(search_include(&include_paths, "libavutil/lfg.h"))
        .header(search_include(&include_paths, "libavutil/log.h"))
        .header(search_include(&include_paths, "libavutil/lzo.h"))
        .header(search_include(&include_paths, "libavutil/macros.h"))
        .header(search_include(&include_paths, "libavutil/mathematics.h"))
        .header(search_include(&include_paths, "libavutil/md5.h"))
        .header(search_include(&include_paths, "libavutil/mem.h"))
        .header(search_include(&include_paths, "libavutil/motion_vector.h"))
        .header(search_include(&include_paths, "libavutil/murmur3.h"))
        .header(search_include(&include_paths, "libavutil/opt.h"))
        .header(search_include(&include_paths, "libavutil/parseutils.h"))
        .header(search_include(&include_paths, "libavutil/pixdesc.h"))
        .header(search_include(&include_paths, "libavutil/pixfmt.h"))
        .header(search_include(&include_paths, "libavutil/random_seed.h"))
        .header(search_include(&include_paths, "libavutil/rational.h"))
        .header(search_include(&include_paths, "libavutil/replaygain.h"))
        .header(search_include(&include_paths, "libavutil/ripemd.h"))
        .header(search_include(&include_paths, "libavutil/samplefmt.h"))
        .header(search_include(&include_paths, "libavutil/sha.h"))
        .header(search_include(&include_paths, "libavutil/sha512.h"))
        .header(search_include(&include_paths, "libavutil/stereo3d.h"))
        .header(search_include(&include_paths, "libavutil/avstring.h"))
        .header(search_include(&include_paths, "libavutil/threadmessage.h"))
        .header(search_include(&include_paths, "libavutil/time.h"))
        .header(search_include(&include_paths, "libavutil/timecode.h"))
        .header(search_include(&include_paths, "libavutil/twofish.h"))
        .header(search_include(&include_paths, "libavutil/avutil.h"))
        .header(search_include(&include_paths, "libavutil/xtea.h"));

    if env::var("CARGO_FEATURE_POSTPROC").is_ok() {
        builder = builder.header(search_include(&include_paths, "libpostproc/postprocess.h"));
    }

    if env::var("CARGO_FEATURE_SWRESAMPLE").is_ok() {
        builder = builder.header(search_include(&include_paths, "libswresample/swresample.h"));
    }

    if env::var("CARGO_FEATURE_SWSCALE").is_ok() {
        builder = builder.header(search_include(&include_paths, "libswscale/swscale.h"));
    }

    // Finish the builder and generate the bindings.
    let bindings = builder.generate()
    // Unwrap the Result and panic on failure.
    .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    bindings
        .write_to_file(output().join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
