# frozen_string_literal: true

require "strscan"

module Wid
  class Lexer
    IDENTIFIER =    /[_A-Za-z][_0-9A-Za-z]*\b/
    INT =           /[-]?(?:[0]|[1-9][0-9]*)/
    FLOAT_DECIMAL = /[.][0-9]+/
    FLOAT_EXP =     /[eE][+-]?[0-9]+/
    NUMERIC =  /#{INT}(#{FLOAT_DECIMAL}#{FLOAT_EXP}|#{FLOAT_DECIMAL}|#{FLOAT_EXP})?/
    WHITESPACE = %r{ [, \c\r\n\t]+ }x.freeze
    # COMMENTS   = %r{ \#.*$ }x

    KEYWORDS = %w[true false nil def end].freeze

    KW_RE = /#{Regexp.union(KEYWORDS.sort)}\b/
    KW_TABLE = Hash[KEYWORDS.map { |kw| [kw, kw.upcase.to_sym] }]

    module Literals
      LCURLY =        '{'
      RCURLY =        '}'
      LPAREN =        '('
      RPAREN =        ')'
      LBRACKET =      '['
      RBRACKET =      ']'
      # COLON =         ':'
      EQUALS =        '='
      BANG =          '!'
      PIPE =          '|'
      AMP =           '&'
    end
    include Literals

    DOT =           '.'

    PUNCTUATION = Regexp.union(Literals.constants.map { |name|
      Literals.const_get(name)
    })

    PUNCTUATION_TABLE = Literals.constants.each_with_object({}) { |x,o|
      o[Literals.const_get(x)] = x
    }

    attr_reader :start

    def initialize(input)
      @input = input
      @scanner = StringScanner.new(input)
    end

    def next_token
      return if @scanner.eos?

      case
      when s = @scanner.scan(WHITESPACE)  then [:WHITESPACE, s]
      # when s = @scanner.scan(COMMENTS)    then [:COMMENT, s]
      when s = @scanner.scan(PUNCTUATION) then [PUNCTUATION_TABLE[s], s]
      when s = @scanner.scan(KW_RE)       then [KW_TABLE[s], s]
      when s = @scanner.scan(IDENTIFIER)  then [:IDENTIFIER, s]
      when s = @scanner.scan(NUMERIC)     then [@scanner[1] ? :FLOAT : :INT, s]
      when s = @scanner.scan(INT)         then [:INT, s]
      when s = @scanner.scan(DOT)         then [:DOT, s]
      else [:UNKNOWN, @scanner.getch]
      end
    end
  end
end
