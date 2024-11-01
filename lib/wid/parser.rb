# frozen_string_literal: true

module Wid
  class Parser
    class StandardError < ::StandardError; end

    class ParseError < StandardError; end

    def initialize(tokens)
      @tokens = tokens
      @current = 0
    end

    def self.parse(tokens)
      new(tokens).parse
    end

    def parse
      statements = []

      statements << statement until eof?

      AST::Node::Program.new(statements: AST::Node::Statements.new(body: statements))
    end

    # Statement → ExpressionStmt | PrintStmt
    def statement
      if match(:PRINT)
        print_statement
      else
        expression
      end
    end

    # print_statement → "print" expression ( "," expression )* ;
    def print_statement
      expressions = []

      # Parse first expression (required)
      expressions << expression

      # Parse additional expressions separated by commas
      expressions << expression while match(:comma)

      # Expect newline or semicolon after print statement
      consume(:"\n", "Expect newline after print statement") unless match(:";")

      AST::Stmt::Print.new(expressions:)
    end

    # expression → equality ;
    def expression
      equality
    end

    # equality → comparison ( ( "!=" | "==" ) comparison )* ;
    def equality
      expr = comparison

      while match(:"!=", :"==")
        operator = previous.type
        right = comparison

        expr = AST::Expr::Binary.new(left: expr, operator:, right:)
      end

      expr
    end

    # comparison → term ( ( ">" | ">=" | "<" | "<=" ) term )* ;
    def comparison
      expr = term

      while match(:>, :">=", :<, :"<=")
        operator = previous.type
        right = term

        expr = AST::Expr::Binary.new(left: expr, operator:, right:)
      end

      expr
    end

    # term → factor ( ( "-" | "+" ) factor )* ;
    def term
      expr = factor

      while match(:-, :+)
        operator = previous.type
        right = factor

        expr = AST::Expr::Binary.new(left: expr, operator:, right:)
      end

      expr
    end

    # factor → unary ( ( "/" | "*" ) unary )* ;
    def factor
      expr = unary

      while match(:/, :*)
        operator = previous.type
        right = unary

        expr = AST::Expr::Binary.new(left: expr, operator:, right:)
      end

      expr
    end

    # unary → ( "!" | "-" ) unary | primary ;
    def unary
      if match(:!, :-)
        operator = previous.type
        right = unary

        return AST::Expr::Unary.new(operator:, right:)
      end

      primary
    end

    # primary → NUMBER | STRING | "true" | "false" | "nil" | "(" expression ")" | IDENTIFIER ;
    def primary
      return number_literal if match(:NUMBER)
      return string_literal if match(:STRING)
      return bool_literal if match(:BOOL)
      return nil_literal if match(:NIL)

      if match(:"(")
        expr = expression
        consume(:")", "Expect ')' after expression.")

        return AST::Expr::Grouping.new(expr:)
      end

      error peek, "Expected a primary expression, got #{peek.inspect}."
    end

    def number_literal
      if previous.value.include?(".")
        ::Wid::AST::Node::Float.new(value: Float(previous.value))
      else
        ::Wid::AST::Node::Integer.new(value: Integer(previous.value))
      end
    end

    def string_literal
      ::Wid::AST::Node::String.new(unescaped: previous.value[1...-1])
    end

    def bool_literal
      ::Wid::AST::Node::Boolean.new(value: previous.value == "true")
    end

    def nil_literal
      ::Wid::AST::Node::Nil.new
    end

    private

    def match(*types)
      types.each do |type|
        if check(type)
          advance
          return true
        end
      end

      false
    end

    def check(type)
      return false if eof?

      peek.type == type
    end

    def advance
      @current += 1 unless eof?

      previous
    end

    def eof?
      # peek.type == :EOF
      peek.nil?
    end

    def peek
      @tokens[@current]
    end

    def previous
      @tokens[@current - 1]
    end

    def consume(type, message)
      return advance if check(type)
      raise error(peek, message)
    end

    def error(token, message)
      raise ParseError.new(message)
    end
  end
end
