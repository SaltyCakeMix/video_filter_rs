use std::{ffi::c_int, time::Instant};

use ffmpeg_the_third::{codec::{self, Parameters}, decoder, encoder, ffi::sws_scale, format::{self, Pixel}, frame, software::scaling::{Context, Flags}, Dictionary, Packet, Rational};
use rusttype::{point, Font, Scale};

struct RenderData {
    f_w: usize,
    f_h: usize,
    dst_w: usize,
    dst_h: usize,
    r_w: usize,
    r_h: usize,
    x: Vec<usize>,
    y: Vec<usize>,
}

impl RenderData {
    fn new(r_w: u32, r_h: u32, dst_w: u32, dst_h: u32, f_w: u32, f_h: u32) -> Self {
        Self {
            f_w: f_w as usize,
            f_h: f_h as usize,
            r_w: r_w as usize,
            r_h: r_h as usize,
            dst_w: dst_w as usize,
            dst_h: dst_h as usize,
            x: (0..r_w).map(|x| (x as f32 * dst_w as f32 / r_w as f32) as usize).collect(),
            y: (0..r_h).map(|x| (x as f32 * dst_h as f32 / r_h as f32) as usize).collect(),
        }
    }
}

fn decode_frames(decoder: &mut decoder::Video, encoder: &mut encoder::Video, scaler: &mut Context, char_set: &[Vec<Vec<u8>>], render_data: &RenderData, template: &frame::Video) {
    let mut frame = frame::Video::empty();
    let lum_to_char = char_set.len() as f32 / 256.;
    while decoder.receive_frame(&mut frame).is_ok() {
        // Scale frame to render resolution
        let mut scaled_frame = frame::Video::new(Pixel::GRAY8, render_data.r_w as u32, render_data.r_h as u32);
        unsafe {
            sws_scale(
                scaler.as_mut_ptr(),
                (*frame.as_ptr()).data.as_ptr() as *const *const _,
                (*frame.as_ptr()).linesize.as_ptr() as *const _,
                0,
                frame.height() as c_int,
                (*scaled_frame.as_mut_ptr()).data.as_ptr(),
                [render_data.r_w, 0, 0, 0, 0, 0, 0, 0].as_ptr() as *mut _,
            );
        }
        let luminosity = scaled_frame.data_mut(0);

        // Render characters on to output frame
        let mut new_frame = template.clone();
        let bytes = new_frame.data_mut(0);
        let mut i = 0;
        for y in render_data.y.iter() {
            for x in render_data.x.iter() {
                let char_index = luminosity[i] as f32 * lum_to_char;
                let stamp = &char_set[char_index as usize];

                let mut start = x + y*render_data.dst_w;
                for line in stamp {
                    bytes[start..(start + render_data.f_w)].copy_from_slice(line);
                    start += render_data.dst_w;
                }
                i += 1;
            }
        }

        new_frame.set_pts(frame.timestamp());
        encoder.send_frame(&new_frame).unwrap();
    }
}

fn encode_frames(encoder: &mut encoder::Video, out_stream_index: usize, in_time_base: Rational, out_time_base: Rational, out_ctx: &mut format::context::Output) {
    let mut encoded = Packet::empty();
    while encoder.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(out_stream_index);
        encoded.rescale_ts(in_time_base, out_time_base);
        encoded.write_interleaved(out_ctx).unwrap();
    } 
}

fn construct_char_set(font_path: &[u8], chars: &str, font_h: u32) -> (u32, Vec<Vec<Vec<u8>>>) {
    // Loads font
    let font = Font::try_from_bytes(font_path).expect("Error constructing Font");

    // Determines proper font scaling
    let func = |height| {
        let scale = Scale::uniform(height);
        let v_metrics = font.v_metrics(scale);
        let glyphs: Vec<_> = font.layout(chars, scale, point(0., v_metrics.ascent)).collect();
        let glyphs_height = glyphs
            .iter()
            .map(|g| g.pixel_bounding_box().unwrap_or_default().height())
            .max()
            .unwrap() as u32;
        (glyphs, glyphs_height)
    };
    let (_, glyphs_height) = func(font_h as f32);
    let adj_factor = font_h as f32 / glyphs_height as f32;

    let (glyphs, glyphs_height) = func(font_h as f32 * adj_factor);
    let glyphs_width = glyphs
        .iter()
        .map(|g| g.pixel_bounding_box().unwrap_or_default().width())
        .max()
        .unwrap() as u32;

    // Render characters
    let mut glyph_bytes: Vec<Vec<Vec<u8>>> = Vec::with_capacity(glyphs.len());
    for glyph in glyphs {
        let mut bytes = vec![vec![0; glyphs_width as usize]; glyphs_height as usize];
        if let Some(bounding_box) = glyph.pixel_bounding_box() {
            let x_pad = (glyphs_width - bounding_box.width() as u32) / 2;
            let y_pad = (glyphs_height - bounding_box.height() as u32) / 2;
            glyph.draw(|x, y, v| {
                bytes[(y + y_pad) as usize][(x + x_pad) as usize] = (v * 256.) as u8;
            });
        }
        glyph_bytes.push(bytes);
    }

    (glyphs_width, glyph_bytes)
}

fn main() {
    let src = "src.mp4";
    let dst = "dst.mp4";
    let dst_h = 1080;
    let render_h = 10;
    let font_path = include_bytes!("../MonospaceTypewriter.ttf");
    let char_set = " .-^:~/*+=?%##&$$@@@@@@@@@@@@";

    let start_t = Instant::now();
    ffmpeg_the_third::init().unwrap();

    // Input
    let mut in_ctx = format::input(src).unwrap();

    // Finds video stream
    let in_stream = in_ctx.streams().best(ffmpeg_the_third::media::Type::Video)
        .expect("Could not find a proper video stream");
    let in_stream_index = in_stream.index();
    let in_time_base = in_stream.time_base();
    
    // Creates decoder
    let decoder_ctx = codec::Context::from_parameters(in_stream.parameters()).unwrap();
    let mut decoder = decoder_ctx.decoder().video().unwrap();
    if decoder.format() != Pixel::YUV420P {
        panic!("Pixel format is not YUV420P");
    };

    // Relevant data
    let src_w = decoder.width();
    let src_h = decoder.height();
    let dst_w = dst_h * src_w / src_h;
    let font_h = dst_h / render_h;
    let src_fmt = decoder.format();
    
    // Font
    let (font_w, char_set) = construct_char_set(font_path, char_set, font_h);
    println!("{}",font_w);
    let render_w = dst_w / font_w;

    // Output
    let mut out_ctx = format::output(dst).unwrap();
    let global_header = out_ctx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    // Creates output stream
    let codec = encoder::find(codec::Id::H264).expect("Couldn't find encoding codec");
    let mut out_stream = out_ctx.add_stream(codec).expect("Couldn't create output stream");
    out_stream.set_time_base(in_time_base);
    let out_stream_index = out_stream.index();

    // Creates encoder
    let mut encoder = codec::context::Context::new_with_codec(codec)
        .encoder().video().unwrap();
    encoder.set_width(dst_w);
    encoder.set_height(dst_h);
    encoder.set_aspect_ratio(decoder.aspect_ratio());
    encoder.set_format(Pixel::YUV420P);
    encoder.set_frame_rate(Some(in_stream.avg_frame_rate()));
    encoder.set_time_base(in_time_base);
    
    let mut x264_opts = Dictionary::new();
    x264_opts.set("preset", "veryslow");
    x264_opts.set("crf", "0");

    if global_header {
        encoder.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    let mut encoder = encoder
        .open_with(x264_opts)
        .expect("error opening x264 with supplied settings");
    out_stream.set_parameters(Parameters::from(&encoder));
    
    let dst_fmt = encoder.format();
    let out_time_base = out_stream.time_base();

    // Parses video
    let mut scaler = Context::get(
        src_fmt,
        src_w, src_h,
        Pixel::GRAY8,
        render_w, render_h,
        Flags::FAST_BILINEAR,
    ).unwrap();

    out_ctx.set_metadata(in_ctx.metadata().to_owned());
    out_ctx.write_header().unwrap();
    
    let render_data = RenderData::new(render_w, render_h, dst_w, dst_h, font_w, font_h);
    println!("{}, {}", render_data.x.len(), render_data.y.len());
    println!("{:?}", render_data.x);
    println!("{:?}", render_data.y);
    let mut template = frame::Video::new(dst_fmt, dst_w, dst_h);
    template.data_mut(1).fill(127);
    template.data_mut(2).fill(127);
    for (stream, packet) in in_ctx.packets().filter_map(Result::ok) {
        if stream.index() == in_stream_index {
            decoder.send_packet(&packet).unwrap();
        }
        decode_frames(&mut decoder, &mut encoder, &mut scaler, &char_set, &render_data, &template);
        encode_frames(&mut encoder, out_stream_index, in_time_base, out_time_base, &mut out_ctx);
    }

    // Close file
    decoder.send_eof().unwrap();
    decode_frames(&mut decoder, &mut encoder, &mut scaler, &char_set, &render_data, &template);
    encoder.send_eof().unwrap();
    encode_frames(&mut encoder, out_stream_index, in_time_base, out_time_base, &mut out_ctx);
    out_ctx.write_trailer().unwrap();

    // let audio_stream = input_context.streams().best(ffmpeg_next::media::Type::Audio).unwrap();
    // let audio_stream_index = audio_stream.index();

    let elapsed_time = start_t.elapsed();
    println!("Took {} seconds.", elapsed_time.as_secs_f32());
}