module Wid
  module Nodes
    class Identifier < Expression
      attr_accessor :name

      # TODO This list is incomplete. Complete after some aspects of the parser become clearer.
      EXPECTED_NEXT_TOKENS = %I[\n + - * / == != > < >= <= && ||].freeze

      def initialize(name)
        @name = name
        super
      end

      def ==(other)
        name == other&.name
      end

      def children
        []
      end

      def expects?(next_token)
        EXPECTED_NEXT_TOKENS.include?(next_token.type)
      end
    end
  end
end
